//! "Set up agent status on this host", asked once per host and remembered,
//! and the exact-diff hook review. Port of `useAgentHostSetup.tsx` and
//! `AgentHookWorkflow.tsx`.
//!
//! The prompt is driven by what the daemon observed, not by whether agents are
//! running. It is asked at most once per host (a recorded "not now" is as
//! durable as a yes) and never about a configuration the host could not read.
//! Every write is bound to the host the user answered about: the identity is
//! captured with the question and checked again before every request, so a host
//! switch in between refuses the write rather than redirecting it.

use std::collections::HashMap;

use desktop_core::app_state::HostSetupDecision;
use gpui_kit::Context;

use super::{
    model::AdapterDescriptor,
    runtime::{AgentRuntime, AgentStatus},
    wire::{HookAction, HookReview},
    wiring::{host_hook_wiring, should_prompt_for_setup},
};

/// One host, named two ways: the durable key a decision is remembered under,
/// and the connection identity every write is checked against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentedHost {
    pub profile_id: String,
    pub identity: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Install,
    Review,
}

/// The question as posed: host, label and paths captured together when it
/// opens, so an answer is an answer to what was on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupQuestion {
    pub id: u64,
    pub host: Option<ConsentedHost>,
    pub label: String,
    pub targets: Vec<AdapterDescriptor>,
    pub activity: Option<Activity>,
    pub error: Option<String>,
}

/// A loaded review and the host its diff describes: one answer about one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReview {
    pub review: HookReview,
    pub host: ConsentedHost,
    pub applying: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct SetupState {
    pub question: Option<SetupQuestion>,
    question_epoch: u64,
    pub review: Option<OpenReview>,
    /// What each host was last answered, as app state holds it.
    pub decisions: HashMap<String, HostSetupDecision>,
    /// False while app state is frozen after a file it could not parse: an
    /// answer that cannot be recorded is not consent.
    pub persistable: bool,
    /// The host label and whether another consent flow owns the modal lane.
    pub host_label: String,
    pub allowed: bool,
    /// The helper install that already asked about agent status, by host.
    pub auto_setup: Option<String>,
    auto_setup_spent: Option<String>,
    asserted: Option<String>,
    reassert_running: bool,
    reassert_attempted: bool,
    prompt_key: Option<(String, bool, Option<HostSetupDecision>)>,
    reassert_key: Option<String>,
}

impl AgentRuntime {
    fn connected(&self) -> bool {
        self.active_scope().is_some()
    }

    fn active_host(&self) -> String {
        self.inputs.focus.host_profile_id.clone()
    }

    fn setup_targets(&self) -> Vec<AdapterDescriptor> {
        host_hook_wiring(self.adapters(&self.active_host())).setup_targets
    }

    pub fn consented_host(&self) -> Option<ConsentedHost> {
        let scope = self.active_scope()?;
        (self.setup.allowed && self.setup.persistable).then(|| ConsentedHost {
            profile_id: scope.host_profile_id.clone(),
            identity: scope.identity(),
        })
    }

    /// Whether Settings and the sidebar line should offer to set this host up.
    pub fn setup_offerable(&self) -> bool {
        self.connected() && self.setup.allowed && !self.setup_targets().is_empty()
    }

    pub fn decision(&self, host_profile_id: &str) -> Option<HostSetupDecision> {
        self.setup.decisions.get(host_profile_id).copied()
    }

    fn record_decision(&mut self, host_profile_id: &str, decision: HostSetupDecision) {
        self.setup
            .decisions
            .insert(host_profile_id.to_owned(), decision);
        self.effects().record_decision(host_profile_id, decision);
    }

    fn status_line(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        cx.emit(AgentStatus(message.into()));
    }

    /// Opens the prompt, from Settings, the sidebar's notice, or by itself.
    pub fn offer_setup(&mut self, cx: &mut Context<Self>) {
        self.setup.question_epoch += 1;
        self.setup.question = Some(SetupQuestion {
            id: self.setup.question_epoch,
            host: self.consented_host(),
            label: self.setup.host_label.clone(),
            targets: self.setup_targets(),
            activity: None,
            error: None,
        });
        cx.notify();
    }

    fn update_question(
        &mut self,
        id: u64,
        update: impl FnOnce(&mut SetupQuestion),
        cx: &mut Context<Self>,
    ) {
        if let Some(question) = self
            .setup
            .question
            .as_mut()
            .filter(|question| question.id == id)
        {
            update(question);
            cx.notify();
        }
    }

    fn close_question(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.setup.question_epoch == id {
            self.setup.question_epoch += 1;
            self.setup.question = None;
            cx.notify();
        }
    }

    /// Re-run after every change of hosts, scopes or snapshots: the effects of
    /// `useAgentHostSetup`, each run only when what it depends on moved.
    pub(super) fn setup_tick(&mut self, cx: &mut Context<Self>) {
        let connected = self.connected();
        let host = self.active_host();
        // A host that goes away takes its question with it, rather than
        // leaving a modal that would act on the next host to connect.
        if (!connected || !self.setup.allowed) && self.setup.question.is_some() {
            self.setup.question_epoch += 1;
            self.setup.question = None;
            cx.notify();
        }
        let decision = self.decision(&host);
        let targets = self.setup_targets();
        let wiring = host_hook_wiring(self.adapters(&host));
        let promptable = connected && self.setup.allowed && should_prompt_for_setup(&wiring);

        let prompt_key = Some((host.clone(), promptable, decision));
        if self.setup.prompt_key != prompt_key {
            self.setup.prompt_key = prompt_key;
            if promptable
                && decision.is_none()
                && self.setup.auto_setup_spent.as_ref() != Some(&host)
            {
                let auto = self.setup.auto_setup.as_ref() == Some(&host);
                match (auto, self.consented_host()) {
                    (true, Some(consented)) => {
                        // The helper install asked this already, naming agent
                        // status; asking twice about one decision is noise.
                        self.setup.auto_setup = None;
                        self.setup.auto_setup_spent = Some(host.clone());
                        let label = self.setup.host_label.clone();
                        self.install(targets.clone(), consented, None, cx, move |this, ok, cx| {
                            this.status_line(
                                if ok {
                                    format!("Agent status hooks installed on {label}.")
                                } else {
                                    format!("Could not install the agent status hooks on {label}.")
                                },
                                cx,
                            );
                        });
                    }
                    _ => self.offer_setup(cx),
                }
            }
        }

        // Consent is to keeping this host set up, not to one set of hook
        // events: a managed event set that grew reads as partial, and this
        // brings an already-consented host up to date. Merge-only and
        // idempotent on the host.
        let target_ids: Vec<&str> = targets.iter().map(|adapter| adapter.id.as_str()).collect();
        let reassert_key = Some(format!(
            "{connected}\0{decision:?}\0{}\0{:?}\0{}\0{}",
            self.setup.persistable,
            self.active_scope().map(|scope| scope.identity()),
            self.setup.allowed,
            target_ids.join(",")
        ));
        if self.setup.reassert_key == reassert_key {
            return;
        }
        self.setup.reassert_key = reassert_key;
        if !connected {
            // A new connection is a new chance, and a restarted tmux server has
            // dropped the in-memory naming.
            self.setup.reassert_running = false;
            self.setup.reassert_attempted = false;
            self.setup.asserted = None;
            return;
        }
        let Some(consented) = self.consented_host() else {
            return;
        };
        if decision != Some(HostSetupDecision::Accepted) {
            return;
        }
        if targets.is_empty() || self.setup.reassert_running || self.setup.reassert_attempted {
            if !self.setup.reassert_running {
                self.assert_naming(&consented, cx);
            }
            return;
        }
        self.setup.reassert_running = true;
        let named = targets
            .iter()
            .map(|adapter| adapter.display_name.as_str())
            .collect::<Vec<_>>()
            .join(" and ");
        self.install(targets, consented, None, cx, move |this, ok, cx| {
            // Once per connection, whatever happened: an install that cannot
            // reach `wired` must not rewrite the configuration in a loop.
            this.setup.reassert_running = false;
            this.setup.reassert_attempted = true;
            this.status_line(
                if ok {
                    format!("Updated the agent status hooks for {named} on this host.")
                } else {
                    format!("Could not update the agent status hooks for {named} on this host.")
                },
                cx,
            );
        });
    }

    /// Once per connection: the naming lives in the tmux server's memory.
    fn assert_naming(&mut self, host: &ConsentedHost, cx: &mut Context<Self>) {
        if self.setup.asserted.as_ref() == Some(&host.identity) {
            return;
        }
        self.setup.asserted = Some(host.identity.clone());
        let request = self.host_naming(HookAction::Install, &host.identity, cx);
        cx.spawn(async move |this, cx| {
            let outcome = request.await;
            this.update(cx, |this, cx| match outcome {
                // A change to the user's tmux server is worth one line; finding
                // their own config already does it is not.
                Ok("applied") => this.status_line(
                    "Recommended tmux window naming applied to this host's tmux server.",
                    cx,
                ),
                Ok(_) => {}
                Err(error) => this.status_line(
                    format!("Recommended tmux window naming was not applied: {error}"),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Installs exactly `targets` on exactly `host`. The decision is recorded
    /// after the first write lands (or when nothing needed writing), against
    /// the host that was written, and never without a write.
    fn install(
        &mut self,
        targets: Vec<AdapterDescriptor>,
        host: ConsentedHost,
        question: Option<u64>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        // This connection's setup is now this install; a migration started on
        // the record would race it for the same files.
        self.setup.reassert_attempted = true;
        if let Some(id) = question {
            self.update_question(
                id,
                |question| {
                    question.activity = Some(Activity::Install);
                    question.error = None;
                },
                cx,
            );
        }
        cx.spawn(async move |this, cx| {
            let mut answered = false;
            let mut failure = None;
            for adapter in targets {
                let review = this.update(cx, |this, cx| {
                    this.review_hooks(&adapter.id, HookAction::Install, &host.identity, cx)
                });
                let Ok(review) = review else { return };
                let review = match review.await {
                    Ok(review) => review,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                };
                // The host's own idempotence answer: a re-run writes nothing.
                if review.already_installed {
                    continue;
                }
                let applied =
                    this.update(cx, |this, cx| this.apply_hooks(&review, &host.identity, cx));
                let Ok(applied) = applied else { return };
                if let Err(error) = applied.await {
                    failure = Some(error);
                    break;
                }
                if !answered {
                    answered = true;
                    this.update(cx, |this, _| {
                        this.record_decision(&host.profile_id, HostSetupDecision::Accepted)
                    })
                    .ok();
                }
            }
            this.update(cx, |this, cx| {
                let ok = failure.is_none();
                if ok && !answered {
                    // A host already current has been agreed to as well.
                    this.record_decision(&host.profile_id, HostSetupDecision::Accepted);
                }
                if let Some(id) = question {
                    match &failure {
                        None => this.close_question(id, cx),
                        Some(error) => {
                            let error = error.clone();
                            this.update_question(id, |question| question.error = Some(error), cx);
                        }
                    }
                    this.update_question(id, |question| question.activity = None, cx);
                }
                // Part of the same answer, and after it: a tmux server that
                // refuses the naming must not lose the hooks. Always sent again,
                // as an uninstall in between took it back off the host.
                if ok || answered {
                    this.setup.asserted = None;
                    this.assert_naming(&host, cx);
                }
                // An adapter installed before the one that failed is wired now.
                this.refresh_snapshot(&host.profile_id, cx);
                done(this, ok, cx);
            })
            .ok();
        })
        .detach();
    }

    /// "Set up this host", for exactly what the question listed.
    pub fn accept_setup(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.setup.question.clone() else {
            return;
        };
        let Some(host) = question.host.clone() else {
            let error = if self.setup.persistable {
                "This host is no longer connected; nothing was changed."
            } else {
                "Your saved settings could not be read, so this answer could not be recorded; nothing was changed."
            };
            self.update_question(
                question.id,
                |question| question.error = Some(error.into()),
                cx,
            );
            return;
        };
        let label = question.label.clone();
        self.install(
            question.targets,
            host,
            Some(question.id),
            cx,
            move |this, ok, cx| {
                if ok {
                    this.status_line(format!("Agent status hooks installed on {label}."), cx);
                }
            },
        );
    }

    /// "Not now": durable, and over an earlier yes too, against the host asked about.
    pub fn decline_setup(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.setup.question.clone() else {
            return;
        };
        if let Some(host) = &question.host {
            self.record_decision(&host.profile_id, HostSetupDecision::Declined);
        }
        self.close_question(question.id, cx);
    }

    /// "Review exact changes…": the first target's diff, bound to the host asked about.
    pub fn review_setup(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.setup.question.clone() else {
            return;
        };
        let Some(first) = question.targets.first().cloned() else {
            return;
        };
        let Some(host) = question.host.clone() else {
            let error = if self.setup.persistable {
                "This host is no longer connected; the review could not be loaded."
            } else {
                "Your saved settings could not be read, so this review could not be bound to a host."
            };
            self.update_question(
                question.id,
                |question| question.error = Some(error.into()),
                cx,
            );
            return;
        };
        self.update_question(
            question.id,
            |question| {
                question.activity = Some(Activity::Review);
                question.error = None;
            },
            cx,
        );
        let request = self.review_hooks(&first.id, HookAction::Install, &host.identity, cx);
        cx.spawn(async move |this, cx| {
            let review = request.await;
            this.update(cx, |this, cx| {
                match review {
                    Ok(review) if this.setup.question_epoch == question.id => {
                        this.open_review(review, host, cx);
                        this.close_question(question.id, cx);
                    }
                    Ok(_) => {}
                    // The prompt stays open and the same button becomes a retry.
                    Err(error) => this.update_question(
                        question.id,
                        |question| question.error = Some(error),
                        cx,
                    ),
                }
                this.update_question(question.id, |question| question.activity = None, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The sidebar's Review/Remove hooks item.
    pub fn review_hooks_for(
        &mut self,
        adapter_id: &str,
        action: HookAction,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self
            .active_scope()
            .filter(|_| self.setup.persistable)
            .map(|scope| ConsentedHost {
                profile_id: scope.host_profile_id.clone(),
                identity: scope.identity(),
            })
        else {
            self.status_line(
                "Reviewing hook changes requires a live authoritative host.",
                cx,
            );
            return;
        };
        let request = self.review_hooks(adapter_id, action, &host.identity, cx);
        cx.spawn(async move |this, cx| {
            let review = request.await;
            this.update(cx, |this, cx| match review {
                Ok(review) => this.open_review(review, host, cx),
                Err(error) => this.status_line(error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn open_review(&mut self, review: HookReview, host: ConsentedHost, cx: &mut Context<Self>) {
        self.setup.review = Some(OpenReview {
            review,
            host,
            applying: false,
            error: None,
        });
        cx.notify();
    }

    pub fn cancel_review(&mut self, cx: &mut Context<Self>) {
        if self
            .setup
            .review
            .as_ref()
            .is_some_and(|review| !review.applying)
        {
            self.setup.review = None;
            cx.notify();
        }
    }

    /// Applies the reviewed diff to the host it was computed on, or refuses.
    pub fn confirm_review(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.setup.review.as_mut().filter(|open| !open.applying) else {
            return;
        };
        open.applying = true;
        open.error = None;
        let (review, host) = (open.review.clone(), open.host.clone());
        cx.notify();
        let request = self.apply_hooks(&review, &host.identity, cx);
        cx.spawn(async move |this, cx| {
            let applied = request.await;
            this.update(cx, |this, cx| {
                match applied {
                    Ok(()) => {
                        let done = match review.action {
                            HookAction::Install => "installed",
                            HookAction::Uninstall => "removed",
                        };
                        this.status_line(
                            format!("{} reviewed hooks {done}.", review.adapter_display_name),
                            cx,
                        );
                        this.hooks_changed(review.action, &host, cx);
                        this.setup.review = None;
                    }
                    Err(error) => {
                        if let Some(open) = this.setup.review.as_mut() {
                            open.error = Some(error);
                        }
                    }
                }
                if let Some(open) = this.setup.review.as_mut() {
                    open.applying = false;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Removing this app's hooks withdraws consent to keep the host set up;
    /// both directions carry the host settings with the hooks.
    fn hooks_changed(&mut self, action: HookAction, host: &ConsentedHost, cx: &mut Context<Self>) {
        self.refresh_snapshot(&host.profile_id, cx);
        self.record_decision(
            &host.profile_id,
            match action {
                HookAction::Install => HostSetupDecision::Accepted,
                HookAction::Uninstall => HostSetupDecision::Declined,
            },
        );
        let request = self.host_naming(action, &host.identity, cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = request.await {
                this.update(cx, |this, cx| this.status_line(error, cx)).ok();
            }
        })
        .detach();
    }
}

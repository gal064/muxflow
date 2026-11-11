//! The agent runtime: every shown host's records, kept current from snapshot
//! requests and pushed events, and what follows from a change: native
//! notifications, sounds and the pane attention ring. Port of
//! `useAgentRuntime.ts`, with the request half of `api.ts`.
//!
//! The runtime knows hosts only as [`RuntimeInputs`], which [`super::start`]
//! derives from the connection controller, so tests drive it directly.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use desktop_core::{
    app_state::{AgentSoundPreferences, AppStateStore, HostSetupDecision, update_app_state},
    connection::agent::AgentCommand,
    notifications,
};
use futures::future::{BoxFuture, FutureExt};
use gpui_kit::{Context, EventEmitter, Task};
use serde_json::Value;

use crate::{
    connection_ui::backend::TerminalBackend, terminal::integration::agents::PaneAttention,
};

use super::{
    list::{needs_attention, next_recent_expiration},
    model::{
        AdapterDescriptor, AgentRecord, AgentStore, AttentionKind, Generation, RuntimeMemory,
        WireEvent, agents_matching_pane, derive_rollups, record_key, scope_key,
    },
    notify::{
        AgentFocus, AgentNativeNotification, NotificationDecision, TransitionContext,
        decide_agent_notification,
    },
    sound,
    wire::{self, HookAction, HookReview, LaunchRequest, RequestScope},
};

/// How the runtime reaches a host: `desktop-core` through the connection
/// backend, or a scripted host in tests.
pub trait AgentTransport {
    fn agent_request(
        &self,
        client_id: String,
        command: AgentCommand,
    ) -> BoxFuture<'static, Result<Value, String>>;
    fn tmux_action(
        &self,
        client_id: String,
        action: Value,
    ) -> BoxFuture<'static, Result<Value, String>>;
}

pub struct BackendTransport(pub Arc<dyn TerminalBackend>);

impl AgentTransport for BackendTransport {
    fn agent_request(
        &self,
        client_id: String,
        command: AgentCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        self.0.agent_request(client_id, command)
    }

    fn tmux_action(
        &self,
        client_id: String,
        action: Value,
    ) -> BoxFuture<'static, Result<Value, String>> {
        self.0.tmux_action(client_id, action)
    }
}

/// What leaves the app when an agent wants a human.
pub trait AgentEffects {
    /// Shows the notification; answers whether it can be clicked.
    fn notify(
        &self,
        notification: &AgentNativeNotification,
    ) -> BoxFuture<'static, Result<bool, String>>;
    /// Plays the cue for an event, under the preferences in force.
    fn play(&self, event: AttentionKind, preferences: AgentSoundPreferences);
    fn now_ms(&self) -> u64;
    /// Remembers a host's setup answer in app state.
    fn record_decision(&self, host_profile_id: &str, decision: HostSetupDecision);
}

pub struct NativeEffects {
    pub notifications: notifications::NativeNotifications,
    pub app_state: Arc<AppStateStore>,
}

impl AgentEffects for NativeEffects {
    fn notify(
        &self,
        notification: &AgentNativeNotification,
    ) -> BoxFuture<'static, Result<bool, String>> {
        let content = serde_json::from_value(notification.to_json());
        let native = self.notifications.clone();
        async move {
            let content = content.map_err(|error| error.to_string())?;
            let receipt = desktop_core::platform::spawn(async move {
                notifications::emit_agent_notification(content, &native).await
            })
            .await
            .map_err(|error| format!("native notification worker failed: {error}"))??;
            Ok(receipt.actionable)
        }
        .boxed()
    }

    fn play(&self, event: AttentionKind, preferences: AgentSoundPreferences) {
        sound::play(event, preferences);
    }

    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64)
    }

    fn record_decision(&self, host_profile_id: &str, decision: HostSetupDecision) {
        let store = self.app_state.clone();
        let host = host_profile_id.to_owned();
        desktop_core::platform::spawn(async move {
            let saved = update_app_state(&store, move |state| {
                state.host_setup.insert(host, decision);
            })
            .await;
            if let Err(error) = saved {
                eprintln!("muxflow: could not save the agent setup answer: {error}");
            }
        });
    }
}

/// Everything the runtime needs to know about the hosts, from the controller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeInputs {
    /// One per shown host with a live, mutable client.
    pub scopes: Vec<RequestScope>,
    /// Every host with a bridge; a host outside this list loses its records.
    pub shown: Vec<String>,
    /// The active host and where the user is in it.
    pub focus: AgentFocus,
}

/// A line for the status channel.
pub struct AgentStatus(pub String);

pub struct AgentRuntime {
    transport: Rc<dyn AgentTransport>,
    effects: Rc<dyn AgentEffects>,
    pub(super) store: AgentStore,
    memory: RuntimeMemory,
    pending_seen: HashSet<String>,
    /// One snapshot request per host: its key and the task that answers it.
    requests: HashMap<String, (String, Task<()>)>,
    /// Bumped to ask a host again with an unchanged scope (after a hook change).
    resnapshot: HashMap<String, u64>,
    pub(super) inputs: RuntimeInputs,
    /// The panes each host's attention ring was last set to.
    attention: HashMap<String, Vec<String>>,
    recent_clock: Option<(u64, Task<()>)>,
    pub(super) setup: super::setup::SetupState,
}

impl EventEmitter<AgentStatus> for AgentRuntime {}

impl AgentRuntime {
    pub fn new(transport: Rc<dyn AgentTransport>, effects: Rc<dyn AgentEffects>) -> Self {
        Self {
            transport,
            effects,
            store: AgentStore::default(),
            memory: RuntimeMemory::default(),
            pending_seen: HashSet::new(),
            requests: HashMap::new(),
            resnapshot: HashMap::new(),
            inputs: RuntimeInputs::default(),
            attention: HashMap::new(),
            recent_clock: None,
            setup: super::setup::SetupState::default(),
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.effects.now_ms()
    }

    pub(super) fn transport(&self) -> Rc<dyn AgentTransport> {
        self.transport.clone()
    }

    pub(super) fn effects(&self) -> Rc<dyn AgentEffects> {
        self.effects.clone()
    }

    fn status(&self, message: String, cx: &mut Context<Self>) {
        cx.emit(AgentStatus(message));
    }

    /// The scope for `host_profile_id`, if it has a live, mutable client.
    pub fn scope(&self, host_profile_id: &str) -> Option<&RequestScope> {
        self.inputs
            .scopes
            .iter()
            .find(|scope| scope.host_profile_id == host_profile_id)
    }

    pub fn active_scope(&self) -> Option<&RequestScope> {
        self.scope(&self.inputs.focus.host_profile_id)
    }

    /// The host's authoritative agents on its current server, loudest first.
    pub fn agents(&self, host_profile_id: &str) -> Vec<&AgentRecord> {
        let server = self
            .scope(host_profile_id)
            .map(|scope| scope.server_identity.as_str());
        self.store.agents_for_scope(host_profile_id, server)
    }

    /// Every shown host's agents.
    pub fn all_agents(&self) -> Vec<&AgentRecord> {
        self.inputs
            .shown
            .iter()
            .flat_map(|host| self.agents(host))
            .collect()
    }

    pub fn adapters(&self, host_profile_id: &str) -> &[AdapterDescriptor] {
        self.store
            .host(host_profile_id)
            .map_or(&[], |host| host.adapters.as_slice())
    }

    /// New hosts, scopes or focus from the controller. Requests a snapshot for
    /// every scope whose key changed and drops the hosts no longer shown.
    pub fn sync(&mut self, inputs: RuntimeInputs, cx: &mut Context<Self>) {
        if inputs != self.inputs {
            self.inputs = inputs;
            self.request_snapshots(cx);
            // The focus moved: the row the palette offers follows it.
            cx.notify();
        }
        self.setup_tick(cx);
    }

    /// Anything that changes what the host would answer without changing the
    /// scope (installing hooks is the one that exists) asks again.
    pub fn refresh_snapshot(&mut self, host_profile_id: &str, cx: &mut Context<Self>) {
        *self
            .resnapshot
            .entry(host_profile_id.to_owned())
            .or_default() += 1;
        self.request_snapshots(cx);
    }

    fn request_snapshots(&mut self, cx: &mut Context<Self>) {
        let shown = self.inputs.shown.clone();
        let wanted: Vec<RequestScope> = self
            .inputs
            .scopes
            .iter()
            .filter(|scope| shown.contains(&scope.host_profile_id))
            .cloned()
            .collect();
        let keys: HashMap<String, String> = wanted
            .iter()
            .map(|scope| {
                let bump = self
                    .resnapshot
                    .get(&scope.host_profile_id)
                    .copied()
                    .unwrap_or(0);
                (
                    scope.host_profile_id.clone(),
                    format!("{}\0{bump}", scope.request_key()),
                )
            })
            .collect();
        let mut changed = false;
        let stale: Vec<String> = self
            .requests
            .iter()
            .filter(|(host, (key, _))| keys.get(*host) != Some(key))
            .map(|(host, _)| host.clone())
            .collect();
        for host in stale {
            // Dropping the task cancels its answer.
            self.requests.remove(&host);
            if !keys.contains_key(&host) && shown.contains(&host) {
                changed |= self.store.disconnect(&host);
            }
        }
        let gone: Vec<String> = self
            .store
            .by_host
            .keys()
            .filter(|host| !shown.contains(host))
            .cloned()
            .collect();
        for host in gone {
            changed |= self.store.remove(&host);
        }
        for scope in wanted {
            if self.requests.contains_key(&scope.host_profile_id) {
                continue;
            }
            let command = scope.command("snapshot", AgentCommand::default());
            let answer = self
                .transport
                .agent_request(scope.client_id.clone(), command);
            let host = scope.host_profile_id.clone();
            let task = cx.spawn(async move |this, cx| {
                let answer = answer.await;
                this.update(cx, |this, cx| this.snapshot_answered(&scope, answer, cx))
                    .ok();
            });
            self.requests
                .insert(host.clone(), (keys[&host].clone(), task));
        }
        if changed {
            self.changed(cx);
        }
    }

    fn snapshot_answered(
        &mut self,
        scope: &RequestScope,
        answer: Result<Value, String>,
        cx: &mut Context<Self>,
    ) {
        let mapped = answer.and_then(|response| {
            let snapshot = response
                .get("snapshot")
                .ok_or("Host omitted the authoritative agent snapshot.")?;
            wire::map_snapshot(&scope.event_scope(), snapshot)
        });
        match mapped {
            Ok(snapshot) => self.accept(
                WireEvent::Snapshot {
                    snapshot,
                    replayed: true,
                },
                cx,
            ),
            Err(error) => self.status(
                format!(
                    "Agent snapshot unavailable for {}: {error}",
                    scope.host_profile_id
                ),
                cx,
            ),
        }
    }

    /// A snapshot or event the bridge pushed.
    pub fn pushed(
        &mut self,
        scope: super::model::EventScope,
        label: &str,
        payload: &str,
        cx: &mut Context<Self>,
    ) {
        let mapped = serde_json::from_str::<Value>(payload)
            .map_err(|error| error.to_string())
            .and_then(|payload| wire::map_pushed(&scope, label, &payload));
        match mapped {
            Ok(Some(event)) => self.accept(event, cx),
            Ok(None) => {}
            Err(error) => eprintln!("muxflow: agent event dropped: {error}"),
        }
    }

    pub fn accept(&mut self, event: WireEvent, cx: &mut Context<Self>) {
        let host = event.host_profile_id().to_owned();
        // A straggler from a bridge being torn down must not resurrect the
        // slice the request side just dropped.
        if !self.inputs.shown.contains(&host) {
            return;
        }
        let previous = self
            .store
            .host(&host)
            .map(|host| host.by_id.clone())
            .unwrap_or_default();
        if !self.store.apply(&event) {
            return;
        }
        let next_records = self
            .store
            .host(&host)
            .map(|host| host.by_id.clone())
            .unwrap_or_default();
        match &event {
            WireEvent::Snapshot { snapshot, .. } => {
                let key = scope_key(&snapshot.host_profile_id, &snapshot.server_identity);
                let (watermark, remembered) = match self.memory.scope(&key) {
                    Some(scope) => (Some(scope.watermark), scope.records.clone()),
                    None => (None, Default::default()),
                };
                match watermark {
                    // First sight of this server: everything in it is old news.
                    None => {
                        for record in &snapshot.agents {
                            self.memory
                                .mark_processed(record_key(record), record.attention_generation);
                        }
                    }
                    Some(watermark) if snapshot.notification_watermark > watermark => {
                        for record in &snapshot.agents {
                            self.process_transition(
                                remembered.get(&record.id),
                                record,
                                false,
                                true,
                                cx,
                            );
                        }
                    }
                    Some(_) => {}
                }
                let watermark = watermark.map_or(snapshot.notification_watermark, |watermark| {
                    watermark.max(snapshot.notification_watermark)
                });
                self.memory.commit_scope(key, watermark, &next_records);
            }
            WireEvent::Upsert {
                scope,
                record,
                replayed,
                ..
            } => {
                let key = scope_key(&scope.host_profile_id, &scope.server_identity);
                let (watermark, previous_record) = match self.memory.scope(&key) {
                    Some(remembered) => (
                        remembered.watermark,
                        remembered.records.get(&record.id).cloned(),
                    ),
                    None => (0, previous.get(&record.id).cloned()),
                };
                self.process_transition(previous_record.as_ref(), record, *replayed, false, cx);
                self.memory.commit_scope(key, watermark, &next_records);
            }
            WireEvent::Retired { scope, .. } => {
                let key = scope_key(&scope.host_profile_id, &scope.server_identity);
                let watermark = self.memory.scope(&key).map_or(0, |scope| scope.watermark);
                self.memory.commit_scope(key, watermark, &next_records);
            }
        }
        self.changed(cx);
    }

    fn process_transition(
        &mut self,
        previous: Option<&AgentRecord>,
        next: &AgentRecord,
        replayed: bool,
        reconciled_snapshot: bool,
        cx: &mut Context<Self>,
    ) {
        let key = record_key(next);
        let mut focus = self.inputs.focus.clone();
        focus.app_focused = cx.active_window().is_some();
        focus.terminal_visible = crate::terminal::integration::agents::terminal_visible(cx);
        let decision = decide_agent_notification(
            previous,
            next,
            &TransitionContext {
                focus,
                replayed,
                previously_notified: self.memory.processed_generation(&key),
                adapters: self.adapters(&next.host_profile_id),
                reconciled_snapshot,
                ..TransitionContext::default()
            },
        );
        if decision == NotificationDecision::Ignore {
            return;
        }
        self.memory.mark_processed(key, next.attention_generation);
        let NotificationDecision::Emit(notification) = decision else {
            return;
        };
        // Read when it plays, so a change in Settings applies to the next cue.
        self.effects
            .play(notification.event, crate::settings::get(cx).agent_sounds);
        let shown = self.effects.notify(&notification);
        cx.spawn(async move |this, cx| {
            // Delivery is attempted once. Persistent in-app attention remains.
            let message = match shown.await {
                Ok(true) => return,
                Ok(false) => "Native notification shown without click actions; in-app attention remains available.".to_owned(),
                Err(error) => format!("Native notification unavailable; in-app attention remains: {error}"),
            };
            this.update(cx, |this, cx| this.status(message, cx)).ok();
        })
        .detach();
    }

    /// The person typed or clicked in `pane_id`: its agents' attention is seen,
    /// each at the exact generation it is at now.
    pub fn acknowledge_pane(
        &mut self,
        host_profile_id: &str,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.scope(host_profile_id).cloned() else {
            return;
        };
        let unseen = agents_matching_pane(self.agents(host_profile_id), pane_id);
        for (agent_id, attention) in unseen {
            // Not keyed by topology: a window rename mid-request is the same mark.
            let key = format!(
                "{}\0{}\0{agent_id}\0{attention}",
                scope.identity(),
                scope.server_identity
            );
            if !self.pending_seen.insert(key.clone()) {
                continue;
            }
            // A refusal is reported by `mark_seen`, and the attention stays visible.
            self.mark_seen(scope.clone(), agent_id, attention, Some(key), cx)
                .detach();
        }
    }

    /// Tells the host one agent's attention generation was seen, and mirrors
    /// the host's answer. Generation 0 is never attention.
    pub fn mark_seen(
        &mut self,
        scope: RequestScope,
        agent_id: String,
        attention: Generation,
        pending: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        if attention == 0 {
            return Task::ready(Ok(()));
        }
        let command = wire::mark_seen_command(&scope, &agent_id, attention);
        let answer = self
            .transport
            .agent_request(scope.client_id.clone(), command);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                if let Some(pending) = pending {
                    this.pending_seen.remove(&pending);
                }
                let event_scope = scope.event_scope();
                let accepted = answer.and_then(|response| wire::map_seen(&event_scope, &response));
                match accepted {
                    Ok((record, sequence)) => {
                        let seen_at = match record.attention_seen_at {
                            0 => this.now_ms(),
                            at => at,
                        };
                        this.accept(
                            WireEvent::Upsert {
                                scope: event_scope.clone(),
                                sequence,
                                record: Box::new(record),
                                retired: Vec::new(),
                                replayed: false,
                            },
                            cx,
                        );
                        if this
                            .store
                            .seen_ack(&event_scope, &agent_id, attention, seen_at)
                        {
                            this.changed(cx);
                        }
                        Ok(())
                    }
                    Err(error) => {
                        this.status(format!("Could not mark agent attention seen: {error}"), cx);
                        Err(error)
                    }
                }
            })
            .map_err(|error| error.to_string())?
        })
    }

    /// One request against the active host, answered on the runtime.
    fn request(
        &self,
        scope: &RequestScope,
        command: AgentCommand,
        cx: &mut Context<Self>,
    ) -> Task<Result<Value, String>> {
        let answer = self
            .transport
            .agent_request(scope.client_id.clone(), command);
        cx.spawn(async move |_, _| answer.await)
    }

    fn live_scope(&self, action: &str) -> Result<RequestScope, String> {
        self.active_scope()
            .cloned()
            .ok_or_else(|| format!("Agent {action} requires a live authoritative host."))
    }

    /// The scope a hook request may use: refused, never redirected, when the
    /// app is on another host than the one the user answered about.
    pub fn consented_scope(
        &self,
        expected_host: &str,
        action: &str,
    ) -> Result<RequestScope, String> {
        let scope = self
            .active_scope()
            .ok_or_else(|| format!("{action} requires a live authoritative host."))?;
        if scope.identity() != expected_host {
            return Err(format!(
                "{action} was answered for a different host than this app is connected to now; nothing was changed."
            ));
        }
        Ok(scope.clone())
    }

    pub fn launch(
        &self,
        request: LaunchRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<Value, String>> {
        match self.live_scope("launch") {
            Ok(scope) => self.request(&scope, wire::launch_command(&scope, &request), cx),
            Err(error) => Task::ready(Err(error)),
        }
    }

    pub fn resume(
        &self,
        agent: &AgentRecord,
        request: LaunchRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<Value, String>> {
        match self.live_scope("resume") {
            Ok(scope) => self.request(&scope, wire::resume_command(&scope, agent, &request), cx),
            Err(error) => Task::ready(Err(error)),
        }
    }

    /// Where the branch opened: its workspace and pane.
    pub fn branch(
        &self,
        agent: &AgentRecord,
        cx: &mut Context<Self>,
    ) -> Task<Result<(String, String), String>> {
        let request = match self.live_scope("branch") {
            Ok(scope) => self.request(&scope, wire::branch_command(&scope, agent), cx),
            Err(error) => return Task::ready(Err(error)),
        };
        cx.spawn(async move |_, _| wire::map_branch(&request.await?))
    }

    pub fn review_hooks(
        &self,
        adapter_id: &str,
        action: HookAction,
        expected_host: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<HookReview, String>> {
        let scope = match self.consented_scope(expected_host, "Hook review") {
            Ok(scope) => scope,
            Err(error) => return Task::ready(Err(error)),
        };
        let display = self
            .adapters(&scope.host_profile_id)
            .iter()
            .find(|adapter| adapter.id == adapter_id)
            .map_or_else(
                || adapter_id.to_owned(),
                |adapter| adapter.display_name.clone(),
            );
        let request = self.request(
            &scope,
            wire::hook_review_command(&scope, adapter_id, action),
            cx,
        );
        cx.spawn(async move |_, _| {
            let mut review = wire::map_hook_review(&request.await?, action)?;
            review.adapter_display_name = display;
            Ok(review)
        })
    }

    pub fn apply_hooks(
        &self,
        review: &HookReview,
        expected_host: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = match self.consented_scope(expected_host, "Hook installation") {
            Ok(scope) => self.request(&scope, wire::hook_apply_command(&scope, review), cx),
            Err(error) => return Task::ready(Err(error)),
        };
        cx.spawn(async move |_, _| request.await.map(|_| ()))
    }

    /// The tmux naming lives in the server's memory rather than a file, so it
    /// is outside the consent rule; but an answer about one host must still
    /// not reach another.
    pub fn host_naming(
        &self,
        action: HookAction,
        expected_host: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<&'static str, String>> {
        let request = match self.consented_scope(expected_host, "Host naming") {
            Ok(scope) => self.request(&scope, wire::host_naming_command(&scope, action), cx),
            Err(error) => return Task::ready(Err(error)),
        };
        cx.spawn(async move |_, _| Ok(wire::map_host_naming(&request.await?)))
    }

    /// The store moved: redraw, re-mark the attention rings, and rearm the
    /// one clock that changes a row with no event at all.
    pub(super) fn changed(&mut self, cx: &mut Context<Self>) {
        let mut rings: HashMap<String, Vec<String>> = HashMap::new();
        for host in &self.inputs.shown {
            let rollups = derive_rollups(self.agents(host));
            let mut panes: Vec<String> = rollups
                .by_pane
                .into_iter()
                .filter(|(_, rollup)| rollup.state.is_some_and(needs_attention))
                .map(|(pane, _)| pane)
                .collect();
            panes.sort();
            rings.insert(host.clone(), panes);
        }
        let hosts: HashSet<String> = rings.keys().chain(self.attention.keys()).cloned().collect();
        for host in hosts {
            let next = rings.remove(&host).unwrap_or_default();
            if self.attention.get(&host) != Some(&next) {
                PaneAttention::set_host(&host, next.clone(), cx);
                if next.is_empty() {
                    self.attention.remove(&host);
                } else {
                    self.attention.insert(host, next);
                }
            }
        }
        self.arm_recent_clock(cx);
        self.setup_tick(cx);
        cx.notify();
    }

    fn arm_recent_clock(&mut self, cx: &mut Context<Self>) {
        let now = self.now_ms();
        let Some(deadline) = next_recent_expiration(self.all_agents(), now) else {
            self.recent_clock = None;
            return;
        };
        if self
            .recent_clock
            .as_ref()
            .is_some_and(|(armed, _)| *armed == deadline)
        {
            return;
        }
        let wait = Duration::from_millis(deadline - now);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.recent_clock = None;
                this.arm_recent_clock(cx);
                cx.notify();
            })
            .ok();
        });
        self.recent_clock = Some((deadline, task));
    }

    /// For tests: what each host's attention ring holds.
    #[cfg(test)]
    pub fn attention_rings(&self) -> &HashMap<String, Vec<String>> {
        &self.attention
    }
}

//! The assistant's state and its question loop. Port of `AiPanel.tsx`'s
//! `send` and its tool rounds.
//!
//! One thread per tab of the strip. At the moment a question is sent the panel
//! snapshots what the other tabs hold and puts it in front of the configured
//! model; the model answers, may ask for tools, and the panel answers every
//! call — asking the user first where the tool says to — until it stops
//! talking or the user stops it. Reading the screen is free; asking to type or
//! to run costs a round, whether the user allowed it or not — the asking is
//! the spend. A spent budget is not an ending: it is a question to the user,
//! parked on the conversation exactly like an approval.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use gpui_kit::{App, AsyncApp, Context, Entity, WeakEntity};
use serde_json::{Value, json};
use tmux_control::{Pane, TmuxSnapshot};

use desktop_core::connection::ConnectionSpec;
use desktop_core::connection::automation::AutomationCommand;

use crate::agents::Agents;
use crate::automations::model::AutomationListing;
use crate::automations::panel::{AutomationTransport as _, BackendTransport};
use crate::connection_ui::connection_reducer::ConnectionPhase;
use crate::connection_ui::controller::ConnectionController;
use crate::connection_ui::host_links::{HostLink, session_windows, show_focus};
use crate::{git, ports, settings, workspaces};

use super::context::{
    AgentRow, AiSource, ContextInput, OpenTab, build_context, build_system_prompt, lifecycle_words,
};
use super::engine::{self, AiEvent, ChatMessage, ChatSettings};
use super::store::{self, ActionOutcome, AiAction, Conversations};
use super::tools::{self, AiToolCall, Preflight, SessionNow};

/// How much of the conversation the model is shown.
const HISTORY_TURNS: usize = 12;

/// Tool rounds one question buys before the panel asks the user whether the
/// run is worth continuing. Only rounds that ask to *do* something — type in
/// a pane, run an automation — spend it; a round that only reads the screen
/// is free, because reading is how the model thinks out loud. A loop that
/// cannot answer is stopped by the user, not by a number.
pub(super) const MAX_TOOL_ROUNDS: u64 = 6;

pub struct AiPanel {
    pub(super) controller: Entity<ConnectionController>,
    conversations: Conversations,
    /// The source checkboxes outlive the segment within the run of the app:
    /// clicking another tab of the right panel unmounts this view, and a
    /// choice about what the model may see is not something to re-make for it.
    sources: Vec<AiSource>,
    /// The approval cards whose full command is shown, by call id. The expand
    /// state exists only while the card does, and two waiting cards do not
    /// expand each other.
    expanded: HashSet<String>,
    /// The panel is asking the host for the two sources it does not already
    /// hold, between pressing Ask and the call opening.
    gathering: bool,
    /// The compose box, made with the first frame: it needs a window.
    pub(super) fields: Option<super::view::Fields>,
    /// The transcript's scroll, and the newest content it was kept at, so the
    /// answer being written stays in view.
    pub(super) scroll: gpui_kit::ScrollHandle,
    pub(super) transcript_mark: Option<(usize, usize)>,
}

impl AiPanel {
    pub fn new(controller: Entity<ConnectionController>) -> Self {
        Self {
            controller,
            conversations: Conversations::default(),
            sources: super::context::AI_SOURCES.to_vec(),
            expanded: HashSet::new(),
            gathering: false,
            fields: None,
            scroll: gpui_kit::ScrollHandle::new(),
            transcript_mark: None,
        }
    }

    pub fn conversations(&self) -> &Conversations {
        &self.conversations
    }

    pub fn sources(&self) -> &[AiSource] {
        &self.sources
    }

    /// Flip one source, keeping the tab's own order, as the React filter did.
    pub fn toggle_source(&mut self, source: AiSource) {
        if let Some(at) = self.sources.iter().position(|chosen| *chosen == source) {
            self.sources.remove(at);
        } else {
            let rank = |chosen: &AiSource| {
                super::context::AI_SOURCES
                    .iter()
                    .position(|candidate| *candidate == *chosen)
                    .unwrap_or(0)
            };
            let at = self
                .sources
                .iter()
                .position(|chosen| rank(chosen) > rank(&source))
                .unwrap_or(self.sources.len());
            self.sources.insert(at, source);
        }
    }

    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    pub fn toggle_expanded(&mut self, id: &str) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_owned());
        }
    }

    pub fn gathering(&self) -> bool {
        self.gathering
    }

    /// The thread the segment is showing: the conversation key for the host
    /// and tmux window on screen. `None` while no window is open — every tab
    /// of the strip keeps its own conversation, and there is no thread
    /// without a tab.
    pub fn current_key(&self, cx: &App) -> Option<String> {
        let controller = self.controller.read(cx);
        let link = controller.active_link()?;
        let window = Self::on_screen_window(&link.host_state.snapshot, link)?;
        Some(store::conversation_key(&link.profile_id, &window.id))
    }

    /// The window on screen, the same derivation the tab strip uses: the
    /// remembered one, else the host's active one, else the first.
    fn on_screen_window(snapshot: &TmuxSnapshot, link: &HostLink) -> Option<tmux_control::Window> {
        let session = link
            .active_session_id
            .as_deref()
            .and_then(|id| link.host_state.session(id))?;
        let windows = session_windows(&snapshot.windows, &session.id);
        link.active_window_id
            .as_deref()
            .and_then(|id| windows.iter().find(|window| window.id == id))
            .or_else(|| windows.iter().find(|window| window.active))
            .or_else(|| windows.first())
            .map(|window| (*window).clone())
    }

    /// The pane the strip paints as focused, host focus and optimistic move
    /// included.
    fn on_screen_pane(snapshot: &TmuxSnapshot, link: &HostLink, window_id: &str) -> Option<String> {
        let mut panes: Vec<Pane> = snapshot
            .panes
            .iter()
            .filter(|pane| pane.window_id == window_id)
            .cloned()
            .collect();
        panes.sort_by_key(|pane| pane.index);
        if let Some(focused) = link.optimistic_pane(window_id) {
            show_focus(&mut panes, focused);
        }
        panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| panes.first())
            .map(|pane| pane.id.clone())
    }

    /// A closed tab's thread is a thread about something that no longer
    /// exists. Pruning waits for a snapshot that proves the host has answered:
    /// the empty snapshot of a reconnect would otherwise clear every thread it
    /// survived.
    pub fn prune(&mut self, cx: &mut Context<Self>) {
        let (profile_id, live) = {
            let controller = self.controller.read(cx);
            let Some(link) = controller.active_link() else {
                return;
            };
            if link.host_state.phase != ConnectionPhase::Connected {
                return;
            }
            let snapshot = &link.host_state.snapshot;
            if snapshot.sessions.is_empty() && snapshot.windows.is_empty() {
                return;
            }
            (
                link.profile_id.clone(),
                snapshot
                    .windows
                    .iter()
                    .map(|window| window.id.clone())
                    .collect::<HashSet<_>>(),
            )
        };
        if self.conversations.prune_host(&profile_id, &live) {
            cx.notify();
        }
    }

    /// The panel's Allow/Deny. Unknown or already-answered ids are ignored.
    pub fn resolve_approval(
        &mut self,
        key: &str,
        id: &str,
        approved: bool,
        cx: &mut Context<Self>,
    ) {
        if self.conversations.resolve_approval(key, id, approved) {
            cx.notify();
        }
    }

    /// The user chose Continue. The store clears the card first: the loop must
    /// not find it still parked when it resumes a moment later.
    pub fn continue_run(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.conversations.continuation_rounds(key).is_some() {
            self.conversations.continue_run(key);
            cx.notify();
        }
    }

    /// Stop the in-flight generation. Walking away from the segment does not:
    /// only this button, or Clear, ends a run.
    pub fn stop(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(entry) = self.conversations.read(key)
            && let Some(cancel) = entry.cancel.clone()
        {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.conversations.end(key, None) {
            cx.notify();
        }
    }

    pub fn clear(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.conversations.clear(key) {
            cx.notify();
        }
    }

    /// The endpoint the panel will call, straight from Settings → Assistant.
    pub fn settings(cx: &App) -> ChatSettings {
        let prefs = settings::get(cx);
        ChatSettings {
            base_url: prefs.ai_base_url.clone(),
            model: prefs.ai_model.clone(),
            api_key: prefs.ai_api_key.clone(),
        }
    }

    /// Ask about this tab. Everything that needs the host happens on the
    /// task: the two sources the panel does not already hold are fetched now,
    /// so the snapshot is as current as the other three, and the answer then
    /// streams without ever holding the main thread.
    pub fn send(&mut self, question: String, cx: &mut Context<Self>) {
        let question = question.trim().to_owned();
        let Some(key) = self.current_key(cx) else {
            return;
        };
        let model = Self::settings(cx);
        if question.is_empty()
            || !model.configured()
            || self.gathering
            || self
                .conversations
                .read(&key)
                .is_some_and(|entry| entry.streaming)
        {
            return;
        }
        self.gathering = true;
        cx.notify();
        let weak = cx.weak_entity();
        let sources = self.sources.clone();
        let controller = self.controller.clone();
        cx.spawn(async move |_, cx| {
            // The two sources the panel does not already hold: ask the host
            // for the automations now, so the snapshot is as current as the
            // other three. Ports are polled by their own panel and read from
            // it below.
            let automations = {
                let asking = sources.contains(&AiSource::Automations).then(|| {
                    controller.update(cx, |controller, cx| {
                        let link = controller.active_link()?;
                        if link.host_state.phase != ConnectionPhase::Connected {
                            return None;
                        }
                        let client_id = link.client_id.clone()?;
                        let backend = controller
                            .terminal_link(&link.profile_id)?
                            .read(cx)
                            .backend()
                            .clone();
                        Some(BackendTransport(backend).request(
                            client_id,
                            AutomationCommand {
                                action: "list".into(),
                                ..Default::default()
                            },
                        ))
                    })
                });
                match asking {
                    Some(Some(answer)) => match answer.await.and_then(|value| {
                        serde_json::from_value::<AutomationListing>(value)
                            .map_err(|error| format!("unreadable automation listing: {error}"))
                    }) {
                        Ok(listing) => (Some(listing), None),
                        Err(reason) => (None, Some(reason)),
                    },
                    _ => (None, None),
                }
            };
            // The plan is made on the main thread: it reads every tab.
            let plan = weak.update(cx, |panel, cx| {
                panel.gathering = false;
                let plan = panel.plan_run(&key, &question, &model, &sources, automations, cx);
                cx.notify();
                plan
            });
            if let Ok(Some(plan)) = plan {
                AiPanel::run(plan, weak, cx).await;
            }
        })
        .detach();
    }

    /// Opens the question and assembles the first call: the system prompt with
    /// the context of every source that was on, the recent history with the
    /// assistant's own actions folded into its words, and the question.
    fn plan_run(
        &mut self,
        key: &str,
        question: &str,
        model: &ChatSettings,
        sources: &[AiSource],
        automations: (Option<AutomationListing>, Option<String>),
        cx: &mut Context<Self>,
    ) -> Option<RunPlan> {
        let input = self.gather_context(cx, automations)?;
        let profile_id = self.controller.read(cx).active_profile_id();
        let (history, generation) = self.conversations.begin(key, question);
        let mut messages = vec![ChatMessage {
            role: "system".into(),
            content: Some(build_system_prompt(&build_context(sources, &input))),
            tool_call_id: None,
            tool_calls: None,
        }];
        for turn in history.iter().rev().take(HISTORY_TURNS).rev() {
            if turn.text.is_empty() && turn.actions.is_empty() {
                continue;
            }
            let content = if turn.actions.is_empty() {
                turn.text.clone()
            } else {
                format!(
                    "{}\n\n(You did: {})",
                    turn.text,
                    turn.actions
                        .iter()
                        .map(|action| format!(
                            "{} — {} ({})",
                            action.name,
                            action.summary,
                            action.outcome.label()
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            };
            messages.push(ChatMessage::text(&turn.role, content));
        }
        messages.push(ChatMessage::text("user", question));
        Some(RunPlan {
            key: key.to_owned(),
            generation,
            model: model.clone(),
            profile_id,
            messages,
        })
    }

    /// Everything the tabs hold, read on the main thread at the moment the
    /// question is sent. `None` when no host is on screen at all.
    fn gather_context(
        &self,
        cx: &mut Context<Self>,
        automations: (Option<AutomationListing>, Option<String>),
    ) -> Option<ContextInput> {
        let (automations, automations_error) = automations;
        let controller = self.controller.read(cx);
        let link = controller.active_link()?;
        let host_label = controller.active_host_label();
        let transport = match link.connection {
            ConnectionSpec::Local => "local",
            ConnectionSpec::Ssh { .. } => "ssh",
        };
        let snapshot = link.host_state.snapshot.clone();
        let window = Self::on_screen_window(&snapshot, link);
        let session = link
            .active_session_id
            .as_deref()
            .and_then(|id| link.host_state.session(id));
        let active_pane_id = window
            .as_ref()
            .and_then(|window| Self::on_screen_pane(&snapshot, link, &window.id));
        let agents = agent_rows(cx, &link.profile_id);
        let git_status = git::current_status(cx);
        let (port_forwards, detected_ports) = cx.try_global::<ports::panel::Ports>().map_or_else(
            || (Vec::new(), None),
            |found| found.0.read(cx).context_state(&link.profile_id),
        );
        let open_tabs = controller
            .app_state()
            .map(|state| {
                workspaces::model::app_tabs_for_workspace(
                    &state,
                    &link.profile_id,
                    link.host_state.server_identity.as_deref(),
                    session
                        .map(|session| session.id.as_str())
                        .unwrap_or_default(),
                )
                .into_iter()
                .map(|tab| OpenTab {
                    kind: match tab.kind {
                        desktop_core::app_state::AppTabKind::File => "file",
                        desktop_core::app_state::AppTabKind::Markdown => "markdown",
                        desktop_core::app_state::AppTabKind::GitDiff => "diff",
                    }
                    .to_owned(),
                    title: tab.title.clone(),
                    resource: tab.resource.clone(),
                })
                .collect()
            })
            .unwrap_or_default();
        let active_root_path = cx
            .try_global::<crate::files::Files>()
            .and_then(|files| files.explorer.read(cx).active_root())
            .map(|root| root.path.clone());
        Some(ContextInput {
            host_label,
            transport,
            connected: link.host_state.phase == ConnectionPhase::Connected,
            snapshot,
            active_session_id: session.map(|session| session.id.clone()),
            active_window_id: window.map(|window| window.id),
            active_pane_id,
            agents,
            open_tabs,
            active_root_path,
            git_status,
            git_error: None,
            port_forwards,
            detected_ports,
            automations,
            automations_error,
        })
    }

    /// The shell's current view, fresh at call time — not the question's
    /// snapshot. The tools act on what is on screen now.
    fn session_now(&self, cx: &App, profile_id: &str) -> Option<SessionNow> {
        let controller = self.controller.read(cx);
        let link = controller.link(profile_id)?;
        let snapshot = link.host_state.snapshot.clone();
        let window = Self::on_screen_window(&snapshot, link);
        let agents = agent_rows(cx, profile_id)
            .into_iter()
            .map(|agent| {
                (
                    agent.window_id,
                    if agent.display_name.is_empty() {
                        agent.adapter_id
                    } else {
                        agent.display_name
                    },
                    agent.lifecycle,
                )
            })
            .collect();
        Some(SessionNow {
            snapshot,
            active_window_id: window.map(|window| window.id),
            agents,
        })
    }

    /// The rounds. This is the React `for (;;)` loop: stream an answer, run
    /// what it asked for, and either end the answer or spend another round.
    async fn run(plan: RunPlan, panel: WeakEntity<AiPanel>, cx: &mut AsyncApp) {
        let RunPlan {
            key,
            generation,
            model,
            profile_id,
            mut messages,
        } = plan;
        let mut bubble_has_text = false;
        let mut rounds_left = MAX_TOOL_ROUNDS;
        loop {
            while rounds_left > 0 {
                if bubble_has_text {
                    mutate(&panel, cx, |panel, _| {
                        panel.conversations.delta(&key, "\n\n")
                    });
                }
                let mut stream = match engine::chat(
                    model.clone(),
                    messages.clone(),
                    Some(tools::tool_table()),
                ) {
                    Ok(stream) => stream,
                    Err(message) => {
                        finish(&panel, cx, &key, Some(message));
                        return;
                    }
                };
                // Hand over the means to stop, now that the round's call exists.
                mutate(&panel, cx, |panel, _| {
                    panel.conversations.attach(&key, stream.cancel_flag())
                });
                let mut text = String::new();
                let mut calls: Vec<AiToolCall> = Vec::new();
                let mut failure: Option<String> = None;
                while let Some(event) = stream.next_event().await {
                    match event {
                        AiEvent::Delta(chunk) => {
                            text.push_str(&chunk);
                            mutate(&panel, cx, |panel, _| {
                                panel.conversations.delta(&key, &chunk)
                            });
                        }
                        AiEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => calls.push(AiToolCall {
                            id,
                            name,
                            arguments,
                        }),
                        AiEvent::Usage { completion_tokens } => {
                            mutate(&panel, cx, |panel, _| {
                                panel.conversations.add_tokens(&key, completion_tokens)
                            });
                        }
                        AiEvent::Error(message) => {
                            // A refusal is terminal: show it now, not when the
                            // call settles.
                            failure = Some(message.clone());
                            finish(&panel, cx, &key, Some(message));
                            break;
                        }
                        AiEvent::Done => break,
                    }
                }
                if !alive(&panel, cx, &key, generation) {
                    stream.cancel();
                    return;
                }
                if failure.is_some() {
                    return;
                }
                if calls.is_empty() {
                    if text.is_empty() && !bubble_has_text {
                        mutate(&panel, cx, |panel, _| {
                            panel.conversations.delta(&key, "(no answer)")
                        });
                    }
                    finish(&panel, cx, &key, None);
                    return;
                }
                if !text.is_empty() {
                    bubble_has_text = true;
                }
                // The model asked for actions: answer every one — asking the
                // user first where the tool says to — and let it see the
                // results.
                let echo: Vec<Value> = calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "function": { "name": call.name, "arguments": call.arguments },
                        })
                    })
                    .collect();
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: (!text.is_empty()).then_some(text.clone()),
                    tool_call_id: None,
                    tool_calls: Some(echo),
                });
                let mut asked_to_act = false;
                for call in &calls {
                    let summary = tools::summarize_call(call);
                    mutate(&panel, cx, |panel, _| {
                        panel
                            .conversations
                            .set_activity(&key, Some(format!("{}: {}", call.name, summary)))
                    });
                    let mut outcome = ActionOutcome::Done;
                    let result;
                    if tools::needs_approval(&call.name) {
                        asked_to_act = true;
                        let waiting = mutate(&panel, cx, |panel, _| {
                            panel.conversations.park_approval(
                                &key,
                                call.id.clone(),
                                call.name.clone(),
                                summary.clone(),
                                tools::call_detail(call),
                            )
                        });
                        let approved = match waiting {
                            Some(Some(receiver)) => receiver.await.unwrap_or(false),
                            _ => false,
                        };
                        if !approved {
                            result = "The user denied this.".to_owned();
                            outcome = ActionOutcome::Denied;
                        } else if !alive(&panel, cx, &key, generation) {
                            return;
                        } else {
                            result = execute(&panel, &profile_id, call, cx).await;
                        }
                    } else {
                        result = execute(&panel, &profile_id, call, cx).await;
                    }
                    if !alive(&panel, cx, &key, generation) {
                        return;
                    }
                    mutate(&panel, cx, |panel, _| {
                        panel.conversations.add_action(
                            &key,
                            AiAction {
                                name: call.name.clone(),
                                summary: summary.clone(),
                                outcome,
                            },
                        )
                    });
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: Some(result),
                        tool_call_id: Some(call.id.clone()),
                        tool_calls: None,
                    });
                }
                mutate(&panel, cx, |panel, _| {
                    panel.conversations.set_activity(&key, None)
                });
                if asked_to_act {
                    rounds_left -= 1;
                }
            }
            // The budget is spent without an answer. Stop asking the model and
            // ask the user instead; Continue spends another budget on the same
            // run.
            let waiting = mutate(&panel, cx, |panel, _| {
                panel.conversations.park_continuation(&key, MAX_TOOL_ROUNDS)
            });
            let Some(Some(resume)) = waiting else { return };
            let keep_going = resume.await.unwrap_or(false);
            if !keep_going || !alive(&panel, cx, &key, generation) {
                return;
            }
            rounds_left = MAX_TOOL_ROUNDS;
        }
    }
}

/// Everything one run owns, decided before the first byte goes out.
struct RunPlan {
    key: String,
    generation: u64,
    model: ChatSettings,
    profile_id: String,
    messages: Vec<ChatMessage>,
}

/// One change to the conversations, with the notify the view needs.
fn mutate<T>(
    panel: &WeakEntity<AiPanel>,
    cx: &mut AsyncApp,
    change: impl FnOnce(&mut AiPanel, &mut Context<AiPanel>) -> T,
) -> Option<T> {
    panel
        .update(cx, |panel, cx| {
            let changed = change(panel, cx);
            cx.notify();
            changed
        })
        .ok()
}

/// False the moment the conversation is cleared, pruned by a window closing,
/// or reopened by a newer question; every await checks it.
fn alive(panel: &WeakEntity<AiPanel>, cx: &mut AsyncApp, key: &str, generation: u64) -> bool {
    mutate(panel, cx, |panel, _| {
        panel.conversations.alive(key, generation)
    })
    .unwrap_or(false)
}

/// Ends the generation and repaints.
fn finish(panel: &WeakEntity<AiPanel>, cx: &mut AsyncApp, key: &str, error: Option<String>) {
    mutate(panel, cx, |panel, _| panel.conversations.end(key, error));
}

/// The host's present agents, as rows the prompt can read.
fn agent_rows(cx: &App, profile_id: &str) -> Vec<AgentRow> {
    let Some(agents) = cx.try_global::<Agents>() else {
        return Vec::new();
    };
    agents
        .runtime
        .read(cx)
        .agents(profile_id)
        .into_iter()
        .filter(|record| record.present)
        .map(|record| AgentRow {
            display_name: record.display_name.clone(),
            adapter_id: record.adapter_id.clone(),
            session_name: record.session_name.clone(),
            window_name: record.window_name.clone(),
            window_id: record.window_id.clone(),
            lifecycle: lifecycle_words(record.lifecycle),
        })
        .collect()
}

/// Run one approved (or approval-free) call and answer the model with plain
/// words. Anything the call cannot do comes back as a refusal the model can
/// read — a tool error is information, not an exception.
async fn execute(
    panel: &WeakEntity<AiPanel>,
    profile_id: &str,
    call: &AiToolCall,
    cx: &mut AsyncApp,
) -> String {
    let args = tools::parse_arguments(call);
    let session = mutate(panel, cx, |panel, cx| panel.session_now(cx, profile_id)).flatten();
    match tools::preflight(call, session.as_ref()) {
        Preflight::Answer(answer) => return answer,
        Preflight::Proceed => {}
    }
    match call.name.as_str() {
        "send_to_pane" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let press_enter = args.get("press_enter").and_then(Value::as_bool) == Some(true);
            let Some(pane) = session.as_ref().and_then(|live| {
                tools::resolve_pane(live, args.get("pane").and_then(Value::as_i64))
            }) else {
                return "There is no such pane in the current tab.".into();
            };
            let sent = mutate(panel, cx, |panel, cx| {
                let controller = panel.controller.read(cx);
                let link = controller.link(profile_id)?;
                let client_id = link.client_id.clone()?;
                if link.host_state.phase != ConnectionPhase::Connected {
                    return None;
                }
                let bytes = if press_enter {
                    format!("{text}\r").into_bytes()
                } else {
                    text.into_bytes()
                };
                controller
                    .terminal_link(profile_id)?
                    .read(cx)
                    .backend()
                    .send_input(client_id, pane.id.clone(), bytes)
                    .ok()
            });
            match sent {
                Some(Some(())) if press_enter => {
                    format!("Typed and submitted in pane #{}.", pane.index)
                }
                Some(Some(())) => {
                    format!(
                        "Typed into pane #{}; the user has not pressed Enter.",
                        pane.index
                    )
                }
                _ => "The host is not connected, so nothing can be typed.".into(),
            }
        }
        "read_pane" => {
            let Some(pane) = session.as_ref().and_then(|live| {
                tools::resolve_pane(live, args.get("pane").and_then(Value::as_i64))
            }) else {
                return "There is no such pane in the current tab.".into();
            };
            let rows = tools::read_rows(&args);
            let read = mutate(panel, cx, |panel, cx| {
                panel
                    .controller
                    .read(cx)
                    .terminal_link(profile_id)
                    .and_then(|terminal| terminal.read(cx).screen_text(&pane.id, rows, cx))
            });
            match read {
                Some(None) | None => {
                    format!(
                        "Pane #{} is not on screen, so there is nothing to read.",
                        pane.index
                    )
                }
                Some(Some(text)) if text.trim().is_empty() => {
                    format!("Pane #{}'s screen is empty.", pane.index)
                }
                Some(Some(text)) => format!("Pane #{}:\n{}", pane.index, tools::screen_tail(&text)),
            }
        }
        "show_pane" => {
            let Some(pane) = session.as_ref().and_then(|live| {
                tools::resolve_pane(live, args.get("pane").and_then(Value::as_i64))
            }) else {
                return "There is no such pane in the current tab.".into();
            };
            let shown = mutate(panel, cx, |_, cx| {
                workspaces::current(cx).map(|shell| {
                    shell.update(cx, |shell, cx| shell.select_pane(profile_id, &pane.id, cx))
                })
            });
            match shown {
                Some(Some(())) => format!("Showing pane #{}.", pane.index),
                _ => "There is no such pane in the current tab.".into(),
            }
        }
        "run_automation" => {
            let id = args
                .get("automation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if id.is_empty() {
                return "No automation id was given.".into();
            }
            run_automation(panel, profile_id, id, cx).await
        }
        other => format!("Unknown tool: {other}"),
    }
}

/// Start one automation now: list first so the answer can name what ran, the
/// way the Automations panel's own Run-now does.
async fn run_automation(
    panel: &WeakEntity<AiPanel>,
    profile_id: &str,
    id: &str,
    cx: &mut AsyncApp,
) -> String {
    let transport = mutate(panel, cx, |panel, cx| {
        let controller = panel.controller.read(cx);
        let link = controller.link(profile_id)?;
        if link.host_state.phase != ConnectionPhase::Connected {
            return None;
        }
        let client_id = link.client_id.clone()?;
        let backend = controller
            .terminal_link(profile_id)?
            .read(cx)
            .backend()
            .clone();
        Some((BackendTransport(backend), client_id))
    });
    let Some((transport, client_id)) = transport.flatten() else {
        return "The host is not connected, so the automation cannot run.".into();
    };
    let listing = match transport
        .request(
            client_id.clone(),
            AutomationCommand {
                action: "list".into(),
                ..Default::default()
            },
        )
        .await
        .and_then(|value| {
            serde_json::from_value::<AutomationListing>(value)
                .map_err(|error| format!("unreadable automation listing: {error}"))
        }) {
        Ok(listing) => listing,
        Err(reason) => return format!("The host refused the listing: {reason}"),
    };
    let Some(summary) = listing
        .automations
        .iter()
        .find(|summary| summary.definition.id == id)
    else {
        return format!("There is no automation with the id \"{id}\".");
    };
    let name = if summary.definition.name.is_empty() {
        id.to_owned()
    } else {
        summary.definition.name.clone()
    };
    let after = match transport
        .request(
            client_id,
            AutomationCommand {
                action: "runNow".into(),
                id: id.to_owned(),
                ..Default::default()
            },
        )
        .await
        .and_then(|value| {
            serde_json::from_value::<AutomationListing>(value)
                .map_err(|error| format!("unreadable automation listing: {error}"))
        }) {
        Ok(listing) => listing,
        Err(reason) => return format!("The host refused the run: {reason}"),
    };
    match after
        .automations
        .iter()
        .find(|summary| summary.definition.id == id)
        .and_then(|summary| summary.recent_runs.first())
    {
        Some(run) => format!(
            "Started run #{} of \"{name}\"; it opens a new tab.",
            run.number
        ),
        None => format!("Asked to run \"{name}\"; the host did not confirm a run."),
    }
}

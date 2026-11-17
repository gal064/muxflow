//! Chat: an agent's pane drawn as a conversation instead of a terminal.
//!
//! A pane whose `chat_adapter_id` is set is a chat for its whole life. The
//! terminal surface mounts a [`ChatPane`] for it in the pane's own box, so the
//! tab, the split and the workspace are what they would be for a terminal.
//! Everything shown is typed data from the host's chat service (`chat.proto`):
//! [`model::Chat`] holds one chat by the client rules written there, and the
//! pane sends every action back as a chat request.
//!
//! # For features that draw beside a chat
//!
//! [`set_details_card`] publishes the session details card. A chat draws what
//! it returns at its top right corner, and moves its column left to make room
//! when the pane is wide enough for both.
//!
//! [`agent_mode`] is whether a host turns the agents started in its terminals
//! into chats, and [`set_agent_mode`] changes it.

mod cards;
mod composer;
mod details;
mod icon;
mod links;
pub mod model;
mod open;
pub mod pane;
mod slash;
mod timeline;
mod view;

#[cfg(test)]
mod tests;

use std::{collections::HashMap, rc::Rc, sync::Arc};

use desktop_core::connection::chat::{
    ChatAction, ChatAgentMode, ChatRequest, ChatResponse, ChatUsage,
};
use gpui_kit::{AnyElement, App, Entity, Global, KeyBinding, Pixels, Window, actions};
use tmux_control::Pane;

use crate::{
    connection_ui::{
        backend::TerminalBackend,
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
    },
    settings::{self, AgentMode},
};

pub use pane::ChatPane;

// Escape with the keyboard anywhere in a chat: see `ChatPane::escape`.
actions!(chat, [Escape]);

/// Focus anywhere in a chat pane.
pub const CONTEXT: &str = "Chat";

/// Whether `pane` is drawn as a chat.
pub fn is_chat(pane: &Pane) -> bool {
    !pane.chat_adapter_id.is_empty()
}

/// Which chat a pane holds: its host's profile, its tmux server, its pane.
pub type ChatKey = (String, String, String);

/// How many chats one window holds open, those on screen among them.
pub const KEPT_CHATS: usize = 8;

/// The chats one window holds open, least recently shown first.
///
/// A chat that leaves the screen stays open at the host and keeps applying
/// its events, so coming back to it shows it as it is at once, with its
/// place, draft and open rows, instead of asking for its snapshot again over
/// a slow link. Each costs about what its conversation weighs (see
/// [`model::Chat`]); [`KEPT_CHATS`] bounds that. One let go closes its chat
/// at the host, as a pane taken off the screen used to.
pub struct KeptChats<P = Entity<ChatPane>>(Vec<(ChatKey, P)>);

impl<P> Default for KeptChats<P> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<P: Clone> KeptChats<P> {
    /// The chat's pane if it is held, now the most recently shown.
    pub fn show(&mut self, key: &ChatKey) -> Option<P> {
        let index = self.0.iter().position(|(kept, _)| kept == key)?;
        let entry = self.0.remove(index);
        let pane = entry.1.clone();
        self.0.push(entry);
        Some(pane)
    }

    /// Holds a chat just put on screen.
    pub fn insert(&mut self, key: ChatKey, pane: P) {
        self.0.retain(|(kept, _)| *kept != key);
        self.0.push((key, pane));
    }

    /// Lets go of every chat `keep` turns down.
    pub fn retain(&mut self, mut keep: impl FnMut(&ChatKey, &P) -> bool) {
        self.0.retain(|(key, pane)| keep(key, pane));
    }

    /// Lets the least recently shown go until no more than [`KEPT_CHATS`]
    /// are held. One on screen is never let go.
    pub fn trim(&mut self, on_screen: impl Fn(&ChatKey) -> bool) {
        let mut over = self.0.len().saturating_sub(KEPT_CHATS);
        self.0.retain(|(key, _)| {
            if over == 0 || on_screen(key) {
                return true;
            }
            over -= 1;
            false
        });
    }
}

/// What the details card is drawn for: one chat, in one pane.
pub struct DetailsContext {
    pub profile_id: String,
    pub server_identity: String,
    /// The tmux pane the chat fills, as the last topology described it.
    pub pane: Pane,
    /// The context window's use, once the provider has reported it.
    pub usage: Option<ChatUsage>,
    /// How wide the chat is. Under [`DETAILS_ROOM`] the card covers the
    /// conversation's right edge instead of sitting beside it.
    pub width: Pixels,
}

/// The least chat width, in CSS pixels, at which the column and the details
/// card fit side by side.
pub const DETAILS_ROOM: f32 = 940.;

/// Draws the details card, or nothing while it is hidden.
pub type DetailsCard = Rc<dyn Fn(&DetailsContext, &mut Window, &mut App) -> Option<AnyElement>>;

struct DetailsSlot(DetailsCard);

impl Global for DetailsSlot {}

/// Publishes the session details card. A chat places what `card` returns 14px
/// from its top right corner, 238px wide; the card draws its own frame.
/// Notify the chat's window when what it would return changes.
pub fn set_details_card(card: DetailsCard, cx: &mut App) {
    cx.set_global(DetailsSlot(card));
    cx.refresh_windows();
}

pub(crate) fn details_card(cx: &App) -> Option<DetailsCard> {
    cx.try_global::<DetailsSlot>().map(|slot| slot.0.clone())
}

/// What each host does with an agent started in a terminal, as its last agent
/// snapshot or its answer to a change said.
#[derive(Default)]
struct AgentModes {
    /// Whether the host makes chats, by profile.
    chat: HashMap<String, bool>,
    /// The change being sent, by profile.
    changing: HashMap<String, ModeChange>,
    next_change: u64,
    remembered_change: u64,
    applying: bool,
    apply_result: Option<String>,
}

#[derive(Clone)]
struct ModeChange {
    profile_id: String,
    client_id: String,
    epoch: u64,
    restores: u64,
    serial: u64,
    chat: bool,
}

impl ModeChange {
    fn current(&self, controller: &ConnectionController) -> bool {
        controller.link(&self.profile_id).is_some_and(|link| {
            link.client_id.as_deref() == Some(self.client_id.as_str())
                && link.connection_epoch == self.epoch
                && controller.link_restores(&self.profile_id) == self.restores
                && link.host_state.phase == ConnectionPhase::Connected
        })
    }
}

impl Global for AgentModes {}

pub(crate) fn apply_result(cx: &App) -> Option<&str> {
    cx.try_global::<AgentModes>()?.apply_result.as_deref()
}

struct Chats {
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
}

impl Global for Chats {}

/// Whether `profile_id`'s host turns new agents into chats. `None` until the
/// host has said.
pub fn agent_mode(profile_id: &str, cx: &App) -> Option<bool> {
    let modes = cx.try_global::<AgentModes>()?;
    modes
        .changing
        .get(profile_id)
        .map(|change| change.chat)
        .or_else(|| modes.chat.get(profile_id).copied())
}

/// The connection handshake's effective mode, before its client is writable.
pub(crate) fn host_mode_received(profile_id: &str, chat: bool, cx: &mut App) {
    cx.default_global::<AgentModes>()
        .chat
        .insert(profile_id.to_owned(), chat);
}

/// The `agentMode` of an agent snapshot.
fn snapshot_mode(payload: &str) -> Option<bool> {
    let snapshot: serde_json::Value = serde_json::from_str(payload).ok()?;
    match snapshot.get("agentMode")?.as_str()? {
        "chat" => Some(true),
        "terminal" => Some(false),
        _ => None,
    }
}

/// Asks the host on screen to start new agents as chats, or as terminals.
/// Agents already running stay as they are.
pub fn set_agent_mode(chat: bool, cx: &mut App) {
    let Some(chats) = cx.try_global::<Chats>() else {
        return;
    };
    let controller = chats.controller.clone();
    let profile_id = controller.read(cx).active_profile_id();
    if cx.default_global::<AgentModes>().applying {
        return;
    }
    let Some((change, answer)) = begin_mode_change(&profile_id, chat, &controller, cx) else {
        return;
    };
    cx.default_global::<AgentModes>().apply_result = None;
    publish(&controller, cx);
    cx.spawn(async move |cx| {
        let result = answer.await;
        cx.update(|cx| {
            if let Err(error) = finish_mode_change(&change, result, &controller, cx) {
                controller.update(cx, |controller, cx| {
                    controller
                        .report_status(format!("Could not change the agent mode: {error}"), cx)
                });
            }
            publish(&controller, cx);
        });
    })
    .detach();
}

type ModeAnswer = futures::future::BoxFuture<'static, Result<ChatResponse, String>>;

fn begin_mode_change(
    profile_id: &str,
    chat: bool,
    controller: &Entity<ConnectionController>,
    cx: &mut App,
) -> Option<(ModeChange, ModeAnswer)> {
    let link = controller.read(cx).link(profile_id)?;
    if link.host_state.phase != ConnectionPhase::Connected {
        return None;
    }
    let client_id = link.client_id.clone()?;
    let epoch = link.connection_epoch;
    let restores = controller.read(cx).link_restores(profile_id);
    let backend = cx.try_global::<Chats>()?.backend.clone();
    let modes = cx.default_global::<AgentModes>();
    if modes.changing.contains_key(profile_id) {
        return None;
    }
    modes.next_change += 1;
    let change = ModeChange {
        profile_id: profile_id.to_owned(),
        client_id: client_id.clone(),
        epoch,
        restores,
        serial: modes.next_change,
        chat,
    };
    modes.changing.insert(profile_id.to_owned(), change.clone());
    let answer = backend.chat_request(
        client_id,
        ChatRequest {
            action: ChatAction::SetMode.into(),
            mode: if chat {
                ChatAgentMode::Chat
            } else {
                ChatAgentMode::Terminal
            }
            .into(),
            ..Default::default()
        },
    );
    Some((change, answer))
}

fn finish_mode_change(
    change: &ModeChange,
    result: Result<ChatResponse, String>,
    controller: &Entity<ConnectionController>,
    cx: &mut App,
) -> Result<(), String> {
    let current = change.current(controller.read(cx));
    let modes = cx.default_global::<AgentModes>();
    if modes
        .changing
        .get(&change.profile_id)
        .is_none_or(|pending| pending.serial != change.serial)
    {
        return Err("The host connection changed. Try again.".into());
    }
    modes.changing.remove(&change.profile_id);
    if !current {
        return Err("The host connection changed. Try again.".into());
    }
    let response = result.map_err(|error| pane::error_message(&error).to_owned())?;
    let chat = match ChatAgentMode::try_from(response.mode) {
        Ok(ChatAgentMode::Chat) => true,
        Ok(ChatAgentMode::Terminal) => false,
        _ => return Err("The host returned an invalid agent mode.".into()),
    };
    modes.chat.insert(change.profile_id.clone(), chat);
    // Responses from different hosts can arrive in the opposite order to
    // the user's choices. The latest successful choice stays the default.
    if change.serial > modes.remembered_change {
        modes.remembered_change = change.serial;
        settings::remember_agent_mode(chat, cx);
    }
    Ok(())
}

/// An explicit one-time operation. Offline hosts are reported, never queued.
pub(crate) fn confirm_apply_to_all(window: &mut Window, cx: &mut App) {
    let Some(chats) = cx.try_global::<Chats>() else {
        return;
    };
    let controller = chats.controller.clone();
    let Some(chat) = agent_mode(&controller.read(cx).active_profile_id(), cx) else {
        return;
    };
    let hosts = controller
        .read(cx)
        .profiles()
        .iter()
        .map(|host| (host.id.clone(), host.label.clone()))
        .collect::<Vec<_>>();
    let label = if chat { "Chat" } else { "Terminal" };
    let names = hosts
        .iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    crate::connection_ui::dialog::open_confirmation(crate::connection_ui::dialog::Confirmation {
        title: "Apply to all hosts?",
        detail: format!("New agents will open as {label} on: {names}. Agents already running stay as they are.").into(),
        confirm_label: "Apply to all hosts",
        destructive: false,
        on_confirm: Box::new(move |_, cx| apply_to_hosts(chat, hosts.clone(), controller.clone(), cx)),
    }, window, cx);
}

fn apply_to_hosts(
    chat: bool,
    hosts: Vec<(String, String)>,
    controller: Entity<ConnectionController>,
    cx: &mut App,
) {
    if cx.default_global::<AgentModes>().applying {
        return;
    }
    cx.default_global::<AgentModes>().applying = true;
    cx.default_global::<AgentModes>().apply_result = None;
    let mut pending = Vec::new();
    let mut failures = Vec::new();
    for (profile, label) in hosts {
        match begin_mode_change(&profile, chat, &controller, cx) {
            Some((change, answer)) => pending.push(async move { (label, change, answer.await) }),
            None => failures.push(format!("{label}: not connected or a change is in progress")),
        }
    }
    publish(&controller, cx);
    cx.spawn(async move |cx| {
        let answers = futures::future::join_all(pending).await;
        cx.update(|cx| {
            let mut applied = 0;
            for (label, change, result) in answers {
                match finish_mode_change(&change, result, &controller, cx) {
                    Ok(()) => applied += 1,
                    Err(error) => failures.push(format!("{label}: {error}")),
                }
            }
            cx.default_global::<AgentModes>().applying = false;
            let label = if chat { "Chat" } else { "Terminal" };
            let mut message = format!("Applied {label} to {applied} hosts.");
            if !failures.is_empty() {
                message.push_str(&format!(" Could not apply to {}. Reconnect those hosts and choose Apply to all hosts again to retry.", failures.join("; ")));
            }
            cx.default_global::<AgentModes>().apply_result = Some(message.clone());
            if !failures.is_empty() {
                controller.update(cx, |controller, cx| controller.report_status(message, cx));
            }
            publish(&controller, cx);
        });
    }).detach();
}

/// Tells Settings what the host on screen does, so its control follows.
fn publish(controller: &Entity<ConnectionController>, cx: &mut App) {
    let current = controller.read(cx);
    let stale = cx
        .try_global::<AgentModes>()
        .map(|modes| {
            modes
                .changing
                .iter()
                .filter(|(_, change)| !change.current(current))
                .map(|(profile, _)| profile.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for profile in stale {
        cx.default_global::<AgentModes>().changing.remove(&profile);
    }
    let controller = controller.read(cx);
    let profile_id = controller.active_profile_id();
    let connected = controller.active_link().is_some_and(|link| {
        link.host_state.phase == ConnectionPhase::Connected && link.client_id.is_some()
    });
    let mode = AgentMode {
        chat: agent_mode(&profile_id, cx).filter(|_| connected),
        changing: cx
            .try_global::<AgentModes>()
            .is_some_and(|modes| modes.applying || modes.changing.contains_key(&profile_id)),
    };
    if settings::agent_mode(cx) != mode {
        settings::publish_agent_mode(mode, cx);
    }
}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    cx.set_global(AgentModes::default());
    // Below whatever is open inside the chat: a field, a menu or a popover
    // takes Escape first, and the chat has it only when they leave it.
    cx.bind_keys([KeyBinding::new("escape", Escape, Some(CONTEXT))]);
    details::init(cx);
    settings::set_agent_mode_change(Rc::new(|chat, _, cx| set_agent_mode(chat, cx)), cx);
}

/// Called once the connection runs, from [`crate::connection_ui::start`].
pub fn start(
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut App,
) {
    cx.set_global(Chats {
        controller: controller.clone(),
        backend,
    });
    cx.subscribe(&controller, |controller, event, cx| {
        let ConnectionEvent::AgentService(frame) = event else {
            return;
        };
        if frame.scope != "snapshot" {
            return;
        }
        let Some(chat) = snapshot_mode(&frame.payload) else {
            return;
        };
        let modes = cx.default_global::<AgentModes>();
        if modes.chat.insert(frame.profile_id.clone(), chat) != Some(chat) {
            publish(&controller, cx);
        }
    })
    .detach();
    // The host on screen, or whether it is connected, changed.
    cx.observe(&controller, |controller, cx| publish(&controller, cx))
        .detach();
    publish(&controller, cx);
}

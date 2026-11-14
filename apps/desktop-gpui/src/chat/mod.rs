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
mod links;
pub mod model;
mod open;
pub mod pane;
mod slash;
mod view;

#[cfg(test)]
mod tests;

use std::{collections::HashMap, rc::Rc, sync::Arc};

use desktop_core::connection::chat::{ChatAction, ChatAgentMode, ChatRequest, ChatUsage};
use gpui_kit::{AnyElement, App, Entity, Global, Pixels, Window};
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

/// Whether `pane` is drawn as a chat.
pub fn is_chat(pane: &Pane) -> bool {
    !pane.chat_adapter_id.is_empty()
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
    changing: HashMap<String, bool>,
}

impl Global for AgentModes {}

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
        .or_else(|| modes.chat.get(profile_id))
        .copied()
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
    let backend = chats.backend.clone();
    let Some((profile_id, client_id)) = controller.read(cx).active_link().and_then(|link| {
        (link.host_state.phase == ConnectionPhase::Connected)
            .then(|| Some((link.profile_id.clone(), link.client_id.clone()?)))
            .flatten()
    }) else {
        return;
    };
    let mode = if chat {
        ChatAgentMode::Chat
    } else {
        ChatAgentMode::Terminal
    };
    let request = ChatRequest {
        action: ChatAction::SetMode.into(),
        mode: mode.into(),
        ..Default::default()
    };
    let answer = backend.chat_request(client_id, request);
    cx.default_global::<AgentModes>()
        .changing
        .insert(profile_id.clone(), chat);
    publish(&controller, cx);
    cx.spawn(async move |cx| {
        let result = answer.await;
        cx.update(|cx| {
            let modes = cx.default_global::<AgentModes>();
            modes.changing.remove(&profile_id);
            match result {
                Ok(response) => {
                    let chat = response.mode == i32::from(ChatAgentMode::Chat);
                    modes.chat.insert(profile_id.clone(), chat);
                }
                Err(error) => controller.update(cx, |controller, cx| {
                    controller.report_status(
                        format!(
                            "Could not change the agent mode: {}",
                            pane::error_message(&error)
                        ),
                        cx,
                    );
                }),
            }
            publish(&controller, cx);
        });
    })
    .detach();
}

/// Tells Settings what the host on screen does, so its control follows.
fn publish(controller: &Entity<ConnectionController>, cx: &mut App) {
    let controller = controller.read(cx);
    let profile_id = controller.active_profile_id();
    let connected = controller.active_link().is_some_and(|link| {
        link.host_state.phase == ConnectionPhase::Connected && link.client_id.is_some()
    });
    let mode = AgentMode {
        chat: agent_mode(&profile_id, cx).filter(|_| connected),
        changing: cx
            .try_global::<AgentModes>()
            .is_some_and(|modes| modes.changing.contains_key(&profile_id)),
    };
    if settings::agent_mode(cx) != mode {
        settings::publish_agent_mode(mode, cx);
    }
}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    cx.set_global(AgentModes::default());
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

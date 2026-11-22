//! The assistant: the right panel's "AI" segment. Port of `features/ai` and
//! the Tauri app's `src-tauri/src/ai.rs`.
//!
//! One thread per tab of the strip: switching tabs switches conversations, and
//! closing a tab discards its thread. At the moment a question is sent the
//! panel snapshots what the other tabs already hold — the tmux view, the
//! Explorer's root and open documents, the Git status, the port list, the
//! automations — and puts it in front of an OpenAI-compatible model the user
//! configured under Settings → Assistant.
//!
//! The model can act, but only through the tools in [`tools`], and the two
//! that touch anything outside the screen — typing into a pane, starting an
//! automation — park on an approval card first. Nothing is typed anywhere
//! without the user pressing Allow.

pub mod context;
pub mod engine;
mod panel;
mod store;
mod tools;
mod view;

use gpui_kit::{App, AppContext, Entity, Global, IntoElement, div};

use crate::{connection_ui::controller::ConnectionController, ui::chrome::PanelBodies};

use panel::AiPanel;

/// The running panel. Absent until [`start`]; a generation outlives the
/// segment being closed, because the conversations live on the panel.
pub struct Assistant {
    pub panel: Entity<AiPanel>,
}

impl Global for Assistant {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    PanelBodies::register(
        "ai",
        |_, cx| match cx.try_global::<Assistant>() {
            Some(assistant) => assistant.panel.clone().into_any_element(),
            None => div().into_any_element(),
        },
        cx,
    );
}

/// Called once the connection controller exists, before the first window.
pub fn start(controller: Entity<ConnectionController>, cx: &mut App) {
    let panel = cx.new(|_| AiPanel::new(controller));
    cx.set_global(Assistant { panel });
}

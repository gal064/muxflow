//! Agent state on a pane: the ring a pane gets while its agent wants a human,
//! as `.pane-frame.attention` draws it in the Tauri app. The agents feature
//! (Phase 4) decides which panes; the frame only draws what it is told.

use std::collections::HashSet;

use gpui_kit::{App, Global};

/// Panes whose agent is blocked or done, by host profile and pane id.
#[derive(Default)]
pub struct PaneAttention(HashSet<(String, String)>);

impl Global for PaneAttention {}

impl PaneAttention {
    pub fn wants(&self, profile_id: &str, pane_id: &str) -> bool {
        self.0
            .contains(&(profile_id.to_owned(), pane_id.to_owned()))
    }

    /// Replaces one host's set. Every surface showing it redraws.
    pub fn set_host(profile_id: &str, pane_ids: impl IntoIterator<Item = String>, cx: &mut App) {
        let attention = cx.default_global::<Self>();
        attention.0.retain(|(profile, _)| profile != profile_id);
        attention.0.extend(
            pane_ids
                .into_iter()
                .map(|pane_id| (profile_id.to_owned(), pane_id)),
        );
    }
}

/// What the agents feature does when a person types or clicks in a pane: it
/// acknowledges that pane's agent attention, as `acknowledgePane` does in the
/// Tauri app. Set once by the agents feature; the terminal only reports.
pub struct PaneActivity(pub std::rc::Rc<OnActivity>);

type OnActivity = dyn Fn(&str, &str, &mut App);

impl Global for PaneActivity {}

/// Key or pointer activity in `pane_id` on `profile_id`.
pub fn note_activity(profile_id: &str, pane_id: &str, cx: &mut App) {
    if let Some(hook) = cx.try_global::<PaneActivity>().map(|hook| hook.0.clone()) {
        hook(profile_id, pane_id, cx);
    }
}

/// Whether the terminal is on screen or an app tab covers it. A notification
/// about the active pane is dropped only while the pane can be seen.
pub struct TerminalVisibility(pub bool);

impl Global for TerminalVisibility {}

pub fn terminal_visible(cx: &App) -> bool {
    cx.try_global::<TerminalVisibility>()
        .is_none_or(|visible| visible.0)
}

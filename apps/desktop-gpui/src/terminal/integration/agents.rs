//! What the terminal tells the agents feature about a pane: a person typed or
//! clicked in it, and whether the terminal can be seen.

use gpui_kit::{App, Global};

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

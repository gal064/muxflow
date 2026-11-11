//! The rules for how wide each rail is and whether it shows, from
//! `features/shell/types.ts`, `responsiveShell.ts` and `WorkspaceDock.tsx`.
//!
//! Every length here is a CSS pixel at 100% UI scale, as `app-state.json` stores
//! them, and the window width is in those pixels too. [`rails`] is derived and
//! never stored: a narrow window hides a rail without touching the preference,
//! so widening gives back exactly what was open.

use desktop_core::app_state::{AgentSortMode, DockSide, PanelSurface, ShellPreferences};

use crate::theme::metrics::{PANEL_WIDTH, SIDEBAR_WIDTH};

pub const SIDEBAR_MIN_WIDTH: f32 = SIDEBAR_WIDTH;
pub const PANEL_MIN_WIDTH: f32 = 240.;
pub const TERMINAL_MIN_WIDTH: f32 = 480.;
pub const SIDEBAR_MAX_WINDOW_FRACTION: f32 = 1. / 3.;
pub const PANEL_MAX_WINDOW_FRACTION: f32 = 1. / 2.;
pub const MAX_STORED_WIDTH: f32 = 4_000.;
pub const DOCK_DEFAULT_FRACTION: f32 = 0.4;
pub const DOCK_MIN_FRACTION: f32 = 0.2;
pub const DOCK_MAX_FRACTION: f32 = 0.7;
pub const DOCK_MIN_WIDTH: f32 = 280.;
pub const AGENTS_RATIO_DEFAULT: f32 = 0.42;
pub const AGENTS_RATIO_MIN: f32 = 0.15;
pub const AGENTS_RATIO_MAX: f32 = 0.75;

/// Panel surfaces that every build has, after which come enabled extensions.
pub const BUILT_IN_SURFACES: [(&str, &str); 3] =
    [("files", "Files"), ("git", "Git"), ("ports", "Ports")];
const EXTENSION_LABELS: [(&str, &str); 1] = [("automations", "Automations")];

/// The chrome's preferences, in memory and on disk as part of [`ShellPreferences`].
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub sidebar_collapsed: bool,
    pub sidebar_width: f32,
    pub panel_open: bool,
    pub panel_width: f32,
    pub panel_surface: String,
    pub dock_fraction: f32,
    pub agents_ratio: f32,
    pub agent_sort: AgentSortMode,
    pub pinned_only: bool,
}

impl Prefs {
    /// Reads the saved values through the same clamps the frontend applies.
    pub fn from_shell(shell: &ShellPreferences) -> Self {
        Self {
            sidebar_collapsed: shell.sidebar_collapsed,
            sidebar_width: clamped_sidebar_width(shell.sidebar_width.map(|v| v as f32)),
            panel_open: shell.panel_open,
            panel_width: clamped_panel_width(shell.panel_width.map(|v| v as f32)),
            panel_surface: shell.panel_surface.0.clone(),
            dock_fraction: clamped_dock_fraction(shell.dock_fraction.map(|v| v as f32)),
            agents_ratio: clamped_agents_ratio(shell.agents_section_ratio.map(|v| v as f32)),
            agent_sort: shell.agent_sort,
            pinned_only: shell.pinned_only,
        }
    }

    /// Writes these fields over `shell`, leaving every other preference alone.
    pub fn apply_to(&self, shell: &mut ShellPreferences) {
        shell.sidebar_collapsed = self.sidebar_collapsed;
        shell.sidebar_width = Some(f64::from(self.sidebar_width));
        shell.panel_open = self.panel_open;
        shell.panel_width = Some(f64::from(self.panel_width));
        shell.panel_surface = PanelSurface(self.panel_surface.clone());
        shell.dock_fraction = Some(stored_ratio(self.dock_fraction));
        shell.agents_section_ratio = Some(stored_ratio(self.agents_ratio));
        shell.agent_sort = self.agent_sort;
        shell.pinned_only = self.pinned_only;
    }
}

/// A ratio as JSON: `f32` widened to `f64` would store 0.4 as 0.4000000059604645.
fn stored_ratio(value: f32) -> f64 {
    (f64::from(value) * 10_000.).round() / 10_000.
}

pub fn clamped_sidebar_width(value: Option<f32>) -> f32 {
    match value {
        Some(v) if v.is_finite() && v > SIDEBAR_MIN_WIDTH => v.round().min(MAX_STORED_WIDTH),
        _ => SIDEBAR_MIN_WIDTH,
    }
}

pub fn clamped_panel_width(value: Option<f32>) -> f32 {
    match value {
        Some(v) if v.is_finite() => v.round().clamp(PANEL_MIN_WIDTH, MAX_STORED_WIDTH),
        _ => PANEL_WIDTH,
    }
}

pub fn clamped_dock_fraction(value: Option<f32>) -> f32 {
    value
        .filter(|v| v.is_finite())
        .unwrap_or(DOCK_DEFAULT_FRACTION)
        .clamp(DOCK_MIN_FRACTION, DOCK_MAX_FRACTION)
}

pub fn clamped_agents_ratio(value: Option<f32>) -> f32 {
    value
        .filter(|v| v.is_finite())
        .unwrap_or(AGENTS_RATIO_DEFAULT)
        .clamp(AGENTS_RATIO_MIN, AGENTS_RATIO_MAX)
}

/// The sidebar may never take more than a third of the window.
pub fn sidebar_width_for_window(width: f32, window_width: f32) -> f32 {
    let cap = SIDEBAR_MIN_WIDTH.max((window_width * SIDEBAR_MAX_WINDOW_FRACTION).floor());
    clamped_sidebar_width(Some(width)).min(cap)
}

/// The panel may never take more than half the window.
pub fn panel_width_for_window(width: f32, window_width: f32) -> f32 {
    let cap = PANEL_MIN_WIDTH.max((window_width * PANEL_MAX_WINDOW_FRACTION).floor());
    clamped_panel_width(Some(width)).min(cap)
}

/// What the rails do at one window width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rails {
    pub sidebar_width: f32,
    pub panel_width: f32,
    pub sidebar_open: bool,
    pub panel_open: bool,
    pub sidebar_fits: bool,
    pub panel_fits: bool,
    /// The widest a drag may make each rail, so letting go never hides it.
    pub sidebar_max: f32,
    pub panel_max: f32,
}

/// Which rails show. As the window narrows the sidebar goes first, then the
/// panel, because a panel is closed by default so an open one was asked for.
pub fn rails(prefs: &Prefs, window_width: f32) -> Rails {
    let sidebar_width = sidebar_width_for_window(prefs.sidebar_width, window_width);
    let panel_width = panel_width_for_window(prefs.panel_width, window_width);

    let panel_fits = window_width - panel_width >= TERMINAL_MIN_WIDTH;
    let panel_open = prefs.panel_open && panel_fits;
    // Against the wanted panel, not the drawn one, so the sidebar does not
    // reappear in the room a panel just gave up.
    let wanted_panel = if prefs.panel_open { panel_width } else { 0. };
    let sidebar_fits = window_width - sidebar_width - wanted_panel >= TERMINAL_MIN_WIDTH;
    let sidebar_open = !prefs.sidebar_collapsed && sidebar_fits;

    let drawn_panel = if panel_open { panel_width } else { 0. };
    let drawn_sidebar = if sidebar_open { sidebar_width } else { 0. };
    let sidebar_max = SIDEBAR_MIN_WIDTH.max(
        (window_width / 3.)
            .floor()
            .min(window_width - TERMINAL_MIN_WIDTH - drawn_panel),
    );
    let panel_max = PANEL_MIN_WIDTH.max(
        (window_width / 2.)
            .floor()
            .min(window_width - TERMINAL_MIN_WIDTH - drawn_sidebar),
    );

    Rails {
        sidebar_width,
        panel_width,
        sidebar_open,
        panel_open,
        sidebar_fits,
        panel_fits,
        sidebar_max,
        panel_max,
    }
}

/// The docked document's share of a workspace `workspace_width` wide, with a
/// divider dragged to `offset` from its left edge. Kept within the fraction
/// bounds and, where the workspace is wide enough, the pixel floors too.
pub fn dock_fraction_at(workspace_width: f32, offset: f32, side: DockSide) -> f32 {
    if workspace_width <= 0. {
        return DOCK_MIN_FRACTION;
    }
    let ratio = offset / workspace_width;
    let asked = match side {
        DockSide::Right => 1. - ratio,
        DockSide::Left => ratio,
    };
    let low = DOCK_MIN_FRACTION.max(DOCK_MIN_WIDTH / workspace_width);
    let high = DOCK_MAX_FRACTION.min(1. - TERMINAL_MIN_WIDTH / workspace_width);
    let (min, max) = if low <= high {
        (low, high)
    } else {
        (DOCK_MIN_FRACTION, DOCK_MAX_FRACTION)
    };
    asked.clamp(min, max)
}

/// What the agents ordering toggle says for each mode.
pub fn sort_mode_label(mode: AgentSortMode) -> &'static str {
    match mode {
        AgentSortMode::Status => "priority",
        AgentSortMode::Workspace => "workspace",
        AgentSortMode::Pinned => "pinned",
    }
}

pub fn next_sort_mode(mode: AgentSortMode) -> AgentSortMode {
    match mode {
        AgentSortMode::Status => AgentSortMode::Workspace,
        AgentSortMode::Workspace => AgentSortMode::Pinned,
        AgentSortMode::Pinned => AgentSortMode::Status,
    }
}

/// The panel segments to show: the built-ins and the enabled extensions.
/// `enabled` is `None` when the setting has never been written, which is the
/// default list, Automations on.
pub fn panel_surfaces(enabled: Option<&[String]>) -> Vec<(String, String)> {
    let defaults = ["automations".to_owned()];
    let enabled = enabled.unwrap_or(&defaults);
    let extensions = EXTENSION_LABELS
        .iter()
        .filter(|(id, _)| enabled.iter().any(|e| e == id));
    BUILT_IN_SURFACES
        .iter()
        .chain(extensions)
        .map(|(id, label)| ((*id).to_owned(), (*label).to_owned()))
        .collect()
}

/// The surface to show: the saved one, or Files when that one is not showable.
pub fn resolved_surface<'a>(surfaces: &'a [(String, String)], saved: &'a str) -> &'a str {
    if surfaces.iter().any(|(id, _)| id == saved) {
        saved
    } else {
        "files"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> Prefs {
        Prefs::from_shell(&ShellPreferences::default())
    }

    #[test]
    fn saved_widths_are_clamped_like_the_frontend() {
        assert_eq!(clamped_sidebar_width(None), SIDEBAR_MIN_WIDTH);
        assert_eq!(clamped_sidebar_width(Some(100.)), SIDEBAR_MIN_WIDTH);
        assert_eq!(clamped_sidebar_width(Some(f32::NAN)), SIDEBAR_MIN_WIDTH);
        assert_eq!(clamped_sidebar_width(Some(260.6)), 261.);
        assert_eq!(clamped_sidebar_width(Some(9_000.)), MAX_STORED_WIDTH);
        assert_eq!(clamped_panel_width(None), PANEL_WIDTH);
        assert_eq!(clamped_panel_width(Some(100.)), PANEL_MIN_WIDTH);
        assert_eq!(clamped_dock_fraction(None), DOCK_DEFAULT_FRACTION);
        assert_eq!(clamped_dock_fraction(Some(0.9)), DOCK_MAX_FRACTION);
        assert_eq!(clamped_agents_ratio(None), AGENTS_RATIO_DEFAULT);
        assert_eq!(clamped_agents_ratio(Some(0.01)), AGENTS_RATIO_MIN);
    }

    /// Port of `appStateContract.test.ts`'s right panel width cases.
    #[test]
    fn the_panel_keeps_a_saved_width_within_half_the_window() {
        assert_eq!(clamped_panel_width(Some(420.6)), 421.);
        assert_eq!(clamped_panel_width(Some(9_000.)), MAX_STORED_WIDTH);
        assert_eq!(clamped_panel_width(Some(f32::NAN)), PANEL_WIDTH);
        assert_eq!(panel_width_for_window(900., 1_600.), 800.);
        // Half a narrow window is less than the floor, which still holds.
        assert_eq!(panel_width_for_window(300., 400.), PANEL_MIN_WIDTH);
    }

    #[test]
    fn ratios_are_saved_without_float_noise() {
        let mut shell = ShellPreferences::default();
        prefs().apply_to(&mut shell);
        assert_eq!(shell.dock_fraction, Some(0.4));
        assert_eq!(shell.agents_section_ratio, Some(0.42));
    }

    #[test]
    fn a_rail_cannot_exceed_its_share_of_the_window() {
        assert_eq!(sidebar_width_for_window(600., 900.), 300.);
        assert_eq!(sidebar_width_for_window(600., 600.), SIDEBAR_MIN_WIDTH);
        assert_eq!(panel_width_for_window(800., 1_000.), 500.);
    }

    /// `responsiveShell.test`: the sidebar goes first, then the panel.
    #[test]
    fn the_sidebar_goes_before_the_panel() {
        let mut open = prefs();
        open.panel_open = true;

        let wide = rails(&open, 1_100.);
        assert!(wide.sidebar_open && wide.panel_open);

        // 780 fits the panel alone (780 - 300 >= 480), but not the sidebar too.
        let narrower = rails(&open, 780.);
        assert!(narrower.panel_open && !narrower.sidebar_open && !narrower.sidebar_fits);

        let narrowest = rails(&open, 700.);
        assert!(!narrowest.panel_open && !narrowest.sidebar_open);
        assert!(!narrowest.panel_fits);
    }

    #[test]
    fn the_sidebar_does_not_return_in_the_room_a_closing_panel_left() {
        let mut open = prefs();
        open.panel_open = true;
        // At 700 the panel no longer fits; were it ignored, 700 - 240 >= 480 would
        // bring the sidebar back with a panel-shaped hole.
        let rails = rails(&open, 700.);
        assert!(!rails.panel_open && !rails.sidebar_open);
    }

    #[test]
    fn narrow_windows_do_not_change_the_preference() {
        let mut open = prefs();
        open.panel_open = true;
        let before = open.clone();
        let _ = rails(&open, 320.);
        assert_eq!(open, before);
    }

    #[test]
    fn a_collapsed_rail_stays_closed_at_any_width() {
        let mut closed = prefs();
        closed.sidebar_collapsed = true;
        let rails = rails(&closed, 3_000.);
        assert!(!rails.sidebar_open && rails.sidebar_fits);
    }

    #[test]
    fn a_rail_drag_cannot_cross_the_terminal_floor() {
        let mut open = prefs();
        open.panel_open = true;
        let at = rails(&open, 1_280.);
        // 1280 - 480 - 300 (panel) leaves 500, but a third of the window is 426.
        assert_eq!(at.sidebar_max, 426.);
        // 1280 - 480 - 240 (sidebar) leaves 560, but half the window is 640.
        assert_eq!(at.panel_max, 560.);
    }

    #[test]
    fn a_rail_drag_keeps_the_minimum_when_the_window_has_no_room() {
        let at = rails(&prefs(), 320.);
        assert_eq!(at.sidebar_max, SIDEBAR_MIN_WIDTH);
        assert_eq!(at.panel_max, PANEL_MIN_WIDTH);
    }

    /// `WorkspaceDock.test`: pixel floors apply only where the workspace fits both.
    #[test]
    fn the_dock_fraction_keeps_both_floors() {
        assert!((dock_fraction_at(1_000., 600., DockSide::Right) - 0.4).abs() < 0.001);
        assert!((dock_fraction_at(1_000., 400., DockSide::Left) - 0.4).abs() < 0.001);
        // Dragged past the terminal's floor.
        assert!((dock_fraction_at(1_000., 200., DockSide::Right) - 0.52).abs() < 0.001);
        // Dragged past the document's floor.
        assert!((dock_fraction_at(1_000., 990., DockSide::Right) - 0.28).abs() < 0.001);
        // Too narrow for both floors keeps the fraction bounds alone.
        assert!((dock_fraction_at(500., 0., DockSide::Right) - DOCK_MAX_FRACTION).abs() < 0.001);
        assert_eq!(dock_fraction_at(0., 0., DockSide::Left), DOCK_MIN_FRACTION);
    }

    #[test]
    fn surfaces_default_to_automations_and_fall_back_to_files() {
        let defaults = panel_surfaces(None);
        assert_eq!(defaults.len(), 4);
        assert_eq!(defaults[3].0, "automations");
        assert_eq!(panel_surfaces(Some(&[])).len(), 3);
        assert_eq!(
            resolved_surface(&panel_surfaces(Some(&[])), "automations"),
            "files"
        );
        assert_eq!(resolved_surface(&defaults, "automations"), "automations");
    }

    #[test]
    fn sort_modes_cycle_and_label_like_the_frontend() {
        assert_eq!(sort_mode_label(AgentSortMode::Status), "priority");
        assert_eq!(
            next_sort_mode(AgentSortMode::Workspace),
            AgentSortMode::Pinned
        );
        assert_eq!(next_sort_mode(AgentSortMode::Pinned), AgentSortMode::Status);
    }
}

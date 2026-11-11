//! The Settings values with their defaults applied, read from and written back
//! to the shell block of `app-state.json`. Ported from `normalizePersistedAppState`
//! and `defaultShellState` in `features/shell/types.ts`, so a file written by the
//! Tauri app loads the same here.

use std::collections::{BTreeMap, HashSet};

use desktop_core::app_state::{
    AgentSoundMode, AgentSoundPreferences, AppTabViewMode, MarkdownAppearance, PersistedAppState,
    ThemePreference, WorkspaceDefaults,
};

pub const TERMINAL_FONT_SIZE_MIN: u8 = 10;
pub const TERMINAL_FONT_SIZE_MAX: u8 = 20;
const TERMINAL_FONT_SIZE_DEFAULT: u8 = 13;
/// The interface sizes Settings offers, as zoom factors of the whole window.
pub const UI_SCALES: [f64; 5] = [0.9, 1.0, 1.1, 1.2, 1.3];
/// The panel extensions a build has, by id. Adding one is an entry here and in
/// the Workspace tab's list.
pub const AUTOMATIONS_EXTENSION: &str = "automations";
/// The longest a workspace default may be, in characters. The store refuses a
/// whole save over one field above 16 KiB, so a long paste is bounded here and
/// cannot stop everything else from persisting.
const MAX_WORKSPACE_DEFAULT_LENGTH: usize = 2_048;

pub const DEFAULT_SOUNDS: AgentSoundPreferences = AgentSoundPreferences {
    enabled: true,
    blocked: AgentSoundMode::Subtle,
    completed: AgentSoundMode::Subtle,
    volume: 0.45,
};

/// Every preference Settings edits. Readers take it from
/// [`Settings`](super::Settings) and see a change at once.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: ThemePreference,
    pub ui_scale: f64,
    pub terminal_font_size: u8,
    pub copy_on_select: bool,
    pub clean_wrapped_commands: bool,
    pub terminal_application_clipboard: bool,
    pub focus_follows_mouse: bool,
    pub tabs_in_title_bar: bool,
    pub compact_workspaces: bool,
    pub agent_state_glyphs: bool,
    pub markdown_appearance: MarkdownAppearance,
    pub default_markdown_view: AppTabViewMode,
    pub enabled_panel_extensions: Vec<String>,
    pub agent_sounds: AgentSoundPreferences,
    /// By host profile id; a host with nothing to say has no entry.
    pub workspace_defaults: BTreeMap<String, WorkspaceDefaults>,
}

/// A terminal size typed or stored, held to what Settings offers.
pub fn clamped_terminal_font_size(value: f64) -> u8 {
    if !value.is_finite() {
        return TERMINAL_FONT_SIZE_DEFAULT;
    }
    value.round().clamp(
        f64::from(TERMINAL_FONT_SIZE_MIN),
        f64::from(TERMINAL_FONT_SIZE_MAX),
    ) as u8
}

/// One workspace default, trimmed and bounded, or `None` when there is none:
/// an empty field is a cleared setting, not a setting of "".
pub fn usable_workspace_default(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(MAX_WORKSPACE_DEFAULT_LENGTH).collect())
}

impl Prefs {
    pub fn from_state(state: &PersistedAppState) -> Self {
        let shell = &state.shell;
        let sounds = shell.agent_sounds.unwrap_or(DEFAULT_SOUNDS);
        let mut enabled_panel_extensions = shell
            .enabled_panel_extensions
            .clone()
            .unwrap_or_else(|| vec![AUTOMATIONS_EXTENSION.to_owned()]);
        let mut seen = HashSet::new();
        enabled_panel_extensions.retain(|id| !id.is_empty() && seen.insert(id.clone()));
        Self {
            theme: shell.theme.unwrap_or(ThemePreference::System),
            ui_scale: shell
                .ui_scale
                .filter(|scale| UI_SCALES.contains(scale))
                .unwrap_or(1.0),
            terminal_font_size: shell
                .terminal_font_size
                .map_or(TERMINAL_FONT_SIZE_DEFAULT, |size| {
                    clamped_terminal_font_size(f64::from(size))
                }),
            copy_on_select: shell.copy_on_select.unwrap_or(true),
            clean_wrapped_commands: shell.clean_wrapped_commands.unwrap_or(true),
            terminal_application_clipboard: shell.terminal_application_clipboard.unwrap_or(true),
            focus_follows_mouse: shell.focus_follows_mouse,
            tabs_in_title_bar: shell.tabs_in_title_bar.unwrap_or(true),
            compact_workspaces: shell.compact_workspaces.unwrap_or(true),
            agent_state_glyphs: shell.agent_state_glyphs,
            markdown_appearance: shell
                .markdown_appearance
                .unwrap_or(MarkdownAppearance::Default),
            default_markdown_view: shell.default_markdown_view.unwrap_or(AppTabViewMode::Split),
            enabled_panel_extensions,
            agent_sounds: AgentSoundPreferences {
                volume: if sounds.volume.is_finite() {
                    sounds.volume.clamp(0., 1.)
                } else {
                    DEFAULT_SOUNDS.volume
                },
                ..sounds
            },
            workspace_defaults: state
                .workspace_defaults
                .iter()
                .filter_map(|(host, defaults)| {
                    let usable = WorkspaceDefaults {
                        directory: defaults
                            .directory
                            .as_deref()
                            .and_then(usable_workspace_default),
                        startup_command: defaults
                            .startup_command
                            .as_deref()
                            .and_then(usable_workspace_default),
                    };
                    (!host.is_empty() && usable != WorkspaceDefaults::default())
                        .then(|| (host.clone(), usable))
                })
                .collect(),
        }
    }

    /// Writes these values over the saved ones and leaves the rest of the state
    /// (tabs, layout, shortcuts, the screen-reader flag Tauri still stores)
    /// alone.
    pub fn apply_to(&self, state: &mut PersistedAppState) {
        let shell = &mut state.shell;
        shell.theme = Some(self.theme);
        shell.ui_scale = Some(self.ui_scale);
        shell.terminal_font_size = Some(self.terminal_font_size);
        shell.copy_on_select = Some(self.copy_on_select);
        shell.clean_wrapped_commands = Some(self.clean_wrapped_commands);
        shell.terminal_application_clipboard = Some(self.terminal_application_clipboard);
        shell.focus_follows_mouse = self.focus_follows_mouse;
        shell.tabs_in_title_bar = Some(self.tabs_in_title_bar);
        shell.compact_workspaces = Some(self.compact_workspaces);
        shell.agent_state_glyphs = self.agent_state_glyphs;
        shell.markdown_appearance = Some(self.markdown_appearance);
        shell.default_markdown_view = Some(self.default_markdown_view);
        shell.enabled_panel_extensions = Some(self.enabled_panel_extensions.clone());
        shell.agent_sounds = Some(self.agent_sounds);
        state.workspace_defaults = self.workspace_defaults.clone();
    }

    pub fn defaults_for(&self, host_profile_id: &str) -> WorkspaceDefaults {
        self.workspace_defaults
            .get(host_profile_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Edits one host's entry and keeps the other hosts' untouched. An empty
    /// field clears its half, and a host left with nothing drops out of the map.
    pub fn set_workspace_defaults(
        &mut self,
        host_profile_id: &str,
        directory: Option<&str>,
        startup_command: Option<&str>,
    ) {
        let mut merged = self.defaults_for(host_profile_id);
        if let Some(directory) = directory {
            merged.directory = usable_workspace_default(directory);
        }
        if let Some(command) = startup_command {
            merged.startup_command = usable_workspace_default(command);
        }
        if merged == WorkspaceDefaults::default() {
            self.workspace_defaults.remove(host_profile_id);
        } else {
            self.workspace_defaults
                .insert(host_profile_id.to_owned(), merged);
        }
    }

    pub fn panel_extension_enabled(&self, id: &str) -> bool {
        self.enabled_panel_extensions
            .iter()
            .any(|enabled| enabled == id)
    }

    pub fn set_panel_extension(&mut self, id: &str, on: bool) {
        self.enabled_panel_extensions
            .retain(|enabled| enabled != id);
        if on {
            self.enabled_panel_extensions.push(id.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_from(shell: serde_json::Value) -> PersistedAppState {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "appTabs": [],
            "workspaceUi": [],
            "shell": shell,
        }))
        .unwrap()
    }

    #[test]
    fn a_file_with_no_preferences_gives_the_tauri_defaults() {
        let prefs = Prefs::from_state(&state_from(serde_json::json!({})));
        assert_eq!(
            prefs,
            Prefs {
                theme: ThemePreference::System,
                ui_scale: 1.0,
                terminal_font_size: 13,
                copy_on_select: true,
                clean_wrapped_commands: true,
                terminal_application_clipboard: true,
                focus_follows_mouse: false,
                tabs_in_title_bar: true,
                compact_workspaces: true,
                agent_state_glyphs: false,
                markdown_appearance: MarkdownAppearance::Default,
                default_markdown_view: AppTabViewMode::Split,
                enabled_panel_extensions: vec!["automations".to_owned()],
                agent_sounds: DEFAULT_SOUNDS,
                workspace_defaults: BTreeMap::new(),
            }
        );
    }

    #[test]
    fn saved_values_win_and_an_explicit_empty_list_stays_empty() {
        let prefs = Prefs::from_state(&state_from(serde_json::json!({
            "theme": "light",
            "uiScale": 1.2,
            "terminalFontSize": 16,
            "copyOnSelect": false,
            "cleanWrappedCommands": false,
            "terminalApplicationClipboard": false,
            "focusFollowsMouse": true,
            "tabsInTitleBar": false,
            "compactWorkspaces": false,
            "agentStateGlyphs": true,
            "markdownAppearance": "dark",
            "defaultMarkdownView": "source",
            "enabledPanelExtensions": [],
            "agentSounds": { "enabled": false, "blocked": "none", "completed": "subtle", "volume": 0.2 },
        })));
        assert_eq!(prefs.theme, ThemePreference::Light);
        assert_eq!(prefs.ui_scale, 1.2);
        assert_eq!(prefs.terminal_font_size, 16);
        assert!(!prefs.copy_on_select && !prefs.clean_wrapped_commands);
        assert!(!prefs.terminal_application_clipboard && !prefs.tabs_in_title_bar);
        assert!(prefs.focus_follows_mouse && prefs.agent_state_glyphs);
        assert!(!prefs.compact_workspaces);
        assert_eq!(prefs.markdown_appearance, MarkdownAppearance::Dark);
        assert_eq!(prefs.default_markdown_view, AppTabViewMode::Source);
        assert!(prefs.enabled_panel_extensions.is_empty());
        assert!(!prefs.agent_sounds.enabled);
        assert_eq!(prefs.agent_sounds.blocked, AgentSoundMode::None);
    }

    #[test]
    fn a_size_the_dialog_does_not_offer_falls_back_or_clamps() {
        assert_eq!(clamped_terminal_font_size(f64::NAN), 13);
        assert_eq!(clamped_terminal_font_size(3.), 10);
        assert_eq!(clamped_terminal_font_size(99.), 20);
        assert_eq!(clamped_terminal_font_size(14.6), 15);
        let odd = Prefs::from_state(&state_from(serde_json::json!({
            "uiScale": 1.15, "terminalFontSize": 40,
        })));
        assert_eq!((odd.ui_scale, odd.terminal_font_size), (1.0, 20));
        let loud = Prefs::from_state(&state_from(serde_json::json!({
            "agentSounds": { "enabled": true, "blocked": "subtle", "completed": "none", "volume": 7 },
        })));
        assert_eq!(loud.agent_sounds.volume, 1.);
    }

    #[test]
    fn writing_changes_only_the_settings_fields() {
        let mut state = state_from(serde_json::json!({
            "sidebarWidth": 321.0, "terminalScreenReader": true, "panelOpen": true,
        }));
        let mut prefs = Prefs::from_state(&state);
        prefs.theme = ThemePreference::Dark;
        prefs.terminal_font_size = 18;
        prefs.apply_to(&mut state);
        assert_eq!(state.shell.theme, Some(ThemePreference::Dark));
        assert_eq!(state.shell.terminal_font_size, Some(18));
        assert_eq!(state.shell.sidebar_width, Some(321.0));
        assert!(state.shell.panel_open && state.shell.terminal_screen_reader);
        assert_eq!(Prefs::from_state(&state), prefs);
    }

    #[test]
    fn workspace_defaults_are_per_host_trimmed_and_cleared_by_an_empty_field() {
        let mut prefs = Prefs::from_state(&state_from(serde_json::json!({})));
        prefs.set_workspace_defaults("local", Some("  /work  "), None);
        prefs.set_workspace_defaults("ssh-a", None, Some("htop"));
        assert_eq!(
            prefs.defaults_for("local").directory.as_deref(),
            Some("/work")
        );
        assert_eq!(
            prefs.defaults_for("ssh-a").startup_command.as_deref(),
            Some("htop")
        );
        prefs.set_workspace_defaults("local", None, Some("ls"));
        assert_eq!(
            prefs.defaults_for("local").directory.as_deref(),
            Some("/work")
        );
        prefs.set_workspace_defaults("local", Some("   "), Some(""));
        assert!(!prefs.workspace_defaults.contains_key("local"));
        assert!(prefs.workspace_defaults.contains_key("ssh-a"));
        let long = "x".repeat(MAX_WORKSPACE_DEFAULT_LENGTH + 10);
        assert_eq!(
            usable_workspace_default(&long).map(|kept| kept.len()),
            Some(MAX_WORKSPACE_DEFAULT_LENGTH)
        );
    }

    #[test]
    fn a_saved_entry_with_nothing_usable_is_dropped_on_load() {
        let mut state = state_from(serde_json::json!({}));
        state.workspace_defaults.insert(
            "local".into(),
            WorkspaceDefaults {
                directory: Some("   ".into()),
                startup_command: None,
            },
        );
        assert!(Prefs::from_state(&state).workspace_defaults.is_empty());
    }

    #[test]
    fn a_panel_extension_toggles_without_duplicating() {
        let mut prefs = Prefs::from_state(&state_from(serde_json::json!({})));
        assert!(prefs.panel_extension_enabled("automations"));
        prefs.set_panel_extension("automations", false);
        assert!(!prefs.panel_extension_enabled("automations"));
        prefs.set_panel_extension("automations", true);
        prefs.set_panel_extension("automations", true);
        assert_eq!(prefs.enabled_panel_extensions, ["automations"]);
    }
}

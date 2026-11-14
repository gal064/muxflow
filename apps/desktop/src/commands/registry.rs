//! The one command registry: every command's id, title, group, default keys and
//! availability rule. Port of `apps/desktop/src/commands/registry.ts`.

pub type CommandId = &'static str;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requires {
    Session,
    Window,
    Pane,
    Tab,
    Row,
    HostProfile,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandDefinition {
    pub id: CommandId,
    pub title: &'static str,
    pub group: &'static str,
    pub mac: Option<&'static str>,
    pub linux: Option<&'static str>,
    pub mutates: bool,
    pub requires: Option<Requires>,
    /// Routes through the tmux confirmation. `files.delete` and `git.discard`
    /// are deliberately not marked: they carry their own dialog.
    pub destructive: bool,
    /// The positional selectors: bindable, but eighteen near-identical rows
    /// would bury every command the palette exists to surface.
    pub palette_hidden: bool,
    /// The platform's own binding for this chord is the right answer when the
    /// command cannot run, so the chord is left alone instead of swallowed.
    pub native_fallback: bool,
}

const fn command(id: CommandId, title: &'static str, group: &'static str) -> CommandDefinition {
    CommandDefinition {
        id,
        title,
        group,
        mac: None,
        linux: None,
        mutates: false,
        requires: None,
        destructive: false,
        palette_hidden: false,
        native_fallback: false,
    }
}

impl CommandDefinition {
    const fn keys(mut self, mac: &'static str, linux: &'static str) -> Self {
        self.mac = Some(mac);
        self.linux = Some(linux);
        self
    }

    const fn linux_keys(mut self, linux: &'static str) -> Self {
        self.linux = Some(linux);
        self
    }

    const fn mutates(mut self) -> Self {
        self.mutates = true;
        self
    }

    const fn requires(mut self, requires: Requires) -> Self {
        self.requires = Some(requires);
        self
    }

    const fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    const fn hidden(mut self) -> Self {
        self.palette_hidden = true;
        self
    }

    const fn native_fallback(mut self) -> Self {
        self.native_fallback = true;
        self
    }

    /// This command's default key on `platform`, if it has one.
    pub fn default_shortcut(&self, platform: Platform) -> Option<&'static str> {
        match platform {
            Platform::Mac => self.mac,
            Platform::Linux => self.linux,
        }
    }
}

const APPLICATION: &str = "Application";
const VIEW: &str = "View";
const AGENTS: &str = "Agents";
const WORKSPACE: &str = "Workspace";
const TERMINAL_TAB: &str = "Terminal tab";
const PANE: &str = "Pane";
const TERMINAL: &str = "Terminal";
const FILES: &str = "Files";
const SOURCE_CONTROL: &str = "Source control";

/// Registry order is palette order, and the palette's group headings assume one
/// contiguous run per group.
///
/// The Linux keymap is the macOS keymap with ⌘ read as Ctrl. Where that would
/// take Ctrl+D (EOF) or Ctrl+E (end-of-line), or collide with another binding,
/// the Linux default is Ghostty's instead.
#[rustfmt::skip]
pub static COMMANDS: [CommandDefinition; 82] = [
    command("commands.show", "Show command palette", APPLICATION).keys("Meta+K", "Ctrl+K"),
    command("workspaces.switch", "Quick Open…", APPLICATION).keys("Meta+P", "Ctrl+P"),
    command("shortcuts.configure", "Configure keyboard shortcuts", APPLICATION),
    command("settings.show", "Settings", APPLICATION).keys("Meta+,", "Ctrl+,"),
    command("host.delete", "Delete the selected saved host…", APPLICATION).requires(Requires::HostProfile),
    command("view.toggleSidebar", "Toggle sidebar", VIEW).keys("Meta+B", "Ctrl+B"),
    command("view.togglePanel", "Toggle right panel", VIEW).keys("Meta+L", "Ctrl+L"),
    command("view.showFiles", "Show Files", VIEW).keys("Meta+Shift+E", "Ctrl+Shift+E"),
    command("view.showGit", "Show Source Control", VIEW).keys("Meta+Shift+G", "Ctrl+Shift+G"),
    command("view.showPorts", "Show Ports", VIEW),
    command("view.toggleDock", "Toggle file beside the terminal", VIEW),
    command("focus.workspaces", "Focus workspace list", VIEW),
    command("focus.tabs", "Focus tab strip", VIEW),
    command("focus.back", "Back", VIEW).keys("Meta+[", "Ctrl+["),
    command("focus.forward", "Forward", VIEW).keys("Meta+]", "Ctrl+]"),
    command("agents.jumpUnread", "Jump to the agent that needs you", AGENTS).keys("Meta+Shift+U", "Ctrl+Shift+U"),
    command("agents.toggleSort", "Cycle agent ordering (priority → workspace → pinned)", AGENTS),
    command("agents.focusRow", "Focus the selected agent's pane", AGENTS).requires(Requires::Row),
    command("agents.renameRow", "Rename the selected agent's tab…", AGENTS).requires(Requires::Row),
    command("agents.resumeRow", "Resume the selected agent", AGENTS).requires(Requires::Row),
    command("agents.branchRow", "Branch the selected agent's conversation", AGENTS).requires(Requires::Row),
    command("session.new", "New workspace", WORKSPACE).keys("Meta+N", "Ctrl+N").mutates(),
    command("session.rename", "Rename workspace", WORKSPACE).mutates().requires(Requires::Session),
    command("session.moveLeft", "Move workspace up", WORKSPACE).mutates().requires(Requires::Session),
    command("session.moveRight", "Move workspace down", WORKSPACE).mutates().requires(Requires::Session),
    command("workspaces.showPinnedOnly", "Show pinned workspaces only", WORKSPACE),
    command("workspaces.showAll", "Show all workspaces", WORKSPACE),
    command("session.close", "Close workspace…", WORKSPACE).mutates().requires(Requires::Session).destructive(),
    command("tab.previous", "Previous tab", TERMINAL_TAB).keys("Meta+Shift+[", "Ctrl+Shift+[").requires(Requires::Tab),
    command("tab.next", "Next tab", TERMINAL_TAB).keys("Meta+Shift+]", "Ctrl+Shift+]").requires(Requires::Tab),
    command("window.new", "New terminal tab", TERMINAL_TAB).keys("Meta+T", "Ctrl+T").mutates().requires(Requires::Session),
    command("window.rename", "Rename terminal tab", TERMINAL_TAB).mutates().requires(Requires::Window),
    command("window.moveLeft", "Move current tab left", TERMINAL_TAB).requires(Requires::Tab),
    command("window.moveRight", "Move current tab right", TERMINAL_TAB).requires(Requires::Tab),
    command("window.close", "Close current tab", TERMINAL_TAB).keys("Meta+W", "Ctrl+W").requires(Requires::Tab).destructive(),
    // Ctrl+D is EOF, so split right takes Ghostty's Ctrl+Shift+O, which leaves
    // the Meta→Ctrl image of ⌘⇧D free for split down.
    command("pane.splitRight", "Split pane right", PANE).keys("Meta+D", "Ctrl+Shift+O").mutates().requires(Requires::Pane),
    command("pane.splitDown", "Split pane down", PANE).keys("Meta+Shift+D", "Ctrl+Shift+D").mutates().requires(Requires::Pane),
    command("pane.focusLeft", "Focus pane left", PANE).keys("Meta+Alt+ArrowLeft", "Ctrl+Alt+ArrowLeft").mutates().requires(Requires::Pane),
    command("pane.focusRight", "Focus pane right", PANE).keys("Meta+Alt+ArrowRight", "Ctrl+Alt+ArrowRight").mutates().requires(Requires::Pane),
    command("pane.focusUp", "Focus pane up", PANE).keys("Meta+Alt+ArrowUp", "Ctrl+Alt+ArrowUp").mutates().requires(Requires::Pane),
    command("pane.focusDown", "Focus pane down", PANE).keys("Meta+Alt+ArrowDown", "Ctrl+Alt+ArrowDown").mutates().requires(Requires::Pane),
    command("pane.resizeLeft", "Resize pane left", PANE).linux_keys("Ctrl+Shift+ArrowLeft").mutates().requires(Requires::Pane),
    command("pane.resizeRight", "Resize pane right", PANE).linux_keys("Ctrl+Shift+ArrowRight").mutates().requires(Requires::Pane),
    command("pane.resizeUp", "Resize pane up", PANE).linux_keys("Ctrl+Shift+ArrowUp").mutates().requires(Requires::Pane),
    command("pane.resizeDown", "Resize pane down", PANE).linux_keys("Ctrl+Shift+ArrowDown").mutates().requires(Requires::Pane),
    // Ctrl+E is readline's end-of-line; Ghostty's Ctrl+Shift+Enter instead.
    command("pane.zoom", "Toggle pane zoom", PANE).keys("Meta+E", "Ctrl+Shift+Enter").mutates().requires(Requires::Pane),
    command("pane.close", "Close pane…", PANE).mutates().requires(Requires::Pane).destructive(),
    // On Linux Ctrl+C is also the interrupt: see `keymap::shortcut_disposition`.
    command("terminal.copy", "Copy terminal selection", TERMINAL).keys("Meta+C", "Ctrl+C").requires(Requires::Pane).native_fallback(),
    command("terminal.paste", "Paste into terminal", TERMINAL).keys("Meta+V", "Ctrl+V").requires(Requires::Pane).native_fallback(),
    command("terminal.search", "Find in terminal", TERMINAL).keys("Meta+F", "Ctrl+F").requires(Requires::Pane),
    command("terminal.scrollBottom", "Scroll terminal to bottom", TERMINAL).requires(Requires::Pane),
    command("files.open", "Open the selected file", FILES).requires(Requires::Row),
    command("files.rename", "Rename the selected file…", FILES).requires(Requires::Row),
    command("files.move", "Move the selected file…", FILES).requires(Requires::Row),
    command("files.duplicate", "Duplicate the selected file…", FILES).requires(Requires::Row),
    command("files.download", "Download the selected file…", FILES).requires(Requires::Row),
    command("files.delete", "Delete the selected file…", FILES).requires(Requires::Row),
    command("files.newFile", "New file in the Explorer…", FILES).requires(Requires::Row),
    command("files.newFolder", "New folder in the Explorer…", FILES).requires(Requires::Row),
    command("files.refresh", "Refresh the file tree", FILES).requires(Requires::Row),
    command("git.openDiff", "Open the selected change's diff", SOURCE_CONTROL).requires(Requires::Row),
    command("git.stage", "Stage the selected change", SOURCE_CONTROL).requires(Requires::Row),
    command("git.unstage", "Unstage the selected change", SOURCE_CONTROL).requires(Requires::Row),
    command("git.discard", "Discard the selected change…", SOURCE_CONTROL).requires(Requires::Row),
    // ⌘1–9 selects workspaces and ⌃1–9 selects tabs on macOS. Linux follows the
    // ⌘→Ctrl rule for workspaces, which leaves tabs Alt+N, Ghostty's selector.
    command("workspace.select1", "Go to workspace 1", WORKSPACE).keys("Meta+1", "Ctrl+1").hidden(),
    command("workspace.select2", "Go to workspace 2", WORKSPACE).keys("Meta+2", "Ctrl+2").hidden(),
    command("workspace.select3", "Go to workspace 3", WORKSPACE).keys("Meta+3", "Ctrl+3").hidden(),
    command("workspace.select4", "Go to workspace 4", WORKSPACE).keys("Meta+4", "Ctrl+4").hidden(),
    command("workspace.select5", "Go to workspace 5", WORKSPACE).keys("Meta+5", "Ctrl+5").hidden(),
    command("workspace.select6", "Go to workspace 6", WORKSPACE).keys("Meta+6", "Ctrl+6").hidden(),
    command("workspace.select7", "Go to workspace 7", WORKSPACE).keys("Meta+7", "Ctrl+7").hidden(),
    command("workspace.select8", "Go to workspace 8", WORKSPACE).keys("Meta+8", "Ctrl+8").hidden(),
    command("workspace.select9", "Go to workspace 9", WORKSPACE).keys("Meta+9", "Ctrl+9").hidden(),
    command("tab.select1", "Go to tab 1", TERMINAL_TAB).keys("Ctrl+1", "Alt+1").requires(Requires::Session).hidden(),
    command("tab.select2", "Go to tab 2", TERMINAL_TAB).keys("Ctrl+2", "Alt+2").requires(Requires::Session).hidden(),
    command("tab.select3", "Go to tab 3", TERMINAL_TAB).keys("Ctrl+3", "Alt+3").requires(Requires::Session).hidden(),
    command("tab.select4", "Go to tab 4", TERMINAL_TAB).keys("Ctrl+4", "Alt+4").requires(Requires::Session).hidden(),
    command("tab.select5", "Go to tab 5", TERMINAL_TAB).keys("Ctrl+5", "Alt+5").requires(Requires::Session).hidden(),
    command("tab.select6", "Go to tab 6", TERMINAL_TAB).keys("Ctrl+6", "Alt+6").requires(Requires::Session).hidden(),
    command("tab.select7", "Go to tab 7", TERMINAL_TAB).keys("Ctrl+7", "Alt+7").requires(Requires::Session).hidden(),
    command("tab.select8", "Go to tab 8", TERMINAL_TAB).keys("Ctrl+8", "Alt+8").requires(Requires::Session).hidden(),
    command("tab.select9", "Go to tab 9", TERMINAL_TAB).keys("Ctrl+9", "Alt+9").requires(Requires::Session).hidden(),
];

pub fn command_by_id(id: &str) -> Option<&'static CommandDefinition> {
    COMMANDS.iter().find(|command| command.id == id)
}

/// The palette's commands. The shortcut editor reaches all of [`COMMANDS`].
pub fn palette_commands() -> impl Iterator<Item = &'static CommandDefinition> {
    COMMANDS.iter().filter(|command| !command.palette_hidden)
}

/// The 1-based position a positional selector refers to, if it is one.
pub fn selection_index(id: &str, prefix: &str) -> Option<usize> {
    let digit: usize = id.strip_prefix(prefix)?.parse().ok()?;
    (1..=9).contains(&digit).then_some(digit)
}

/// What the screen offers right now. Everything starts false: a feature fills
/// in its part through [`crate::commands::update_context`] as it arrives.
#[derive(Clone, Debug, Default)]
pub struct CommandContext {
    pub can_mutate: bool,
    /// Whether any host offered by the New workspace dialog can create one.
    pub can_create_workspace: bool,
    pub has_session: bool,
    pub has_window: bool,
    pub has_pane: bool,
    pub has_tab: bool,
    pub can_move_session_up: bool,
    pub can_move_session_down: bool,
    pub can_move_tab_left: bool,
    pub can_move_tab_right: bool,
    /// Whether Settings is showing a picked saved host the store will part with.
    pub has_host_profile: bool,
    pub pinned_only: bool,
    /// Whether there is a document to dock beside the terminal, or one to return.
    pub can_toggle_dock: bool,
    /// Whether the active pane holds a selection Linux Ctrl+C should copy.
    pub has_terminal_selection: bool,
    /// Row commands the row surfaces publish for their current rows.
    pub row_commands: Vec<CommandId>,
}

pub fn command_available(command: &CommandDefinition, context: &CommandContext) -> bool {
    // The surface applied every proxy condition before publishing, so its answer
    // is the rule, not an input to one.
    if command.requires == Some(Requires::Row) {
        return context.row_commands.contains(&command.id);
    }
    if command.requires == Some(Requires::HostProfile) {
        return context.has_host_profile;
    }
    match command.id {
        // Each other's inverse, so exactly one of them is ever the thing to run.
        "workspaces.showPinnedOnly" => return !context.pinned_only,
        "workspaces.showAll" => return context.pinned_only,
        // Has its own host picker: available when any shown host is writable,
        // even if the host on screen is not.
        "session.new" => return context.can_create_workspace,
        "view.toggleDock" => return context.can_toggle_dock,
        _ => {}
    }
    if command.mutates && !context.can_mutate {
        return false;
    }
    let window_frozen = context.has_window && !context.can_mutate;
    if command.id == "window.close" && window_frozen {
        return false;
    }
    let requirement_met = match command.requires {
        Some(Requires::Session) => context.has_session,
        Some(Requires::Window) => context.has_window,
        Some(Requires::Pane) => context.has_pane,
        Some(Requires::Tab) => context.has_tab,
        _ => true,
    };
    if !requirement_met {
        return false;
    }
    match command.id {
        "window.moveLeft" => !window_frozen && context.can_move_tab_left,
        "window.moveRight" => !window_frozen && context.can_move_tab_right,
        "session.moveLeft" => context.can_move_session_up,
        "session.moveRight" => context.can_move_session_down,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_id(id: &str) -> &'static CommandDefinition {
        command_by_id(id).unwrap_or_else(|| panic!("no command {id}"))
    }

    fn connected() -> CommandContext {
        CommandContext {
            can_mutate: true,
            can_create_workspace: true,
            has_session: true,
            has_window: true,
            has_pane: true,
            has_tab: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_palette_lists_64_and_hides_only_the_positional_selectors() {
        assert_eq!(COMMANDS.len(), 82);
        assert_eq!(palette_commands().count(), 64);
        for command in COMMANDS.iter().filter(|command| command.palette_hidden) {
            assert!(
                command.id.starts_with("workspace.select") || command.id.starts_with("tab.select"),
                "{} is hidden",
                command.id
            );
        }
    }

    #[test]
    fn group_headings_are_one_contiguous_run() {
        let mut seen: Vec<&str> = Vec::new();
        for command in palette_commands() {
            if seen.last() != Some(&command.group) {
                assert!(!seen.contains(&command.group), "{} split", command.group);
                seen.push(command.group);
            }
        }
    }

    #[test]
    fn offers_new_workspace_when_any_shown_host_can_create() {
        let frozen_active = CommandContext {
            can_mutate: false,
            can_create_workspace: true,
            ..Default::default()
        };
        assert!(command_available(by_id("session.new"), &frozen_active));
        let none = CommandContext {
            can_mutate: true,
            can_create_workspace: false,
            ..Default::default()
        };
        assert!(!command_available(by_id("session.new"), &none));
    }

    #[test]
    fn freezes_mutating_commands_while_disconnected_but_keeps_local_ones() {
        let frozen = CommandContext {
            can_mutate: false,
            ..connected()
        };
        assert!(!command_available(by_id("pane.splitRight"), &frozen));
        assert!(command_available(by_id("view.toggleSidebar"), &frozen));
    }

    #[test]
    fn relative_moves_need_an_adjacent_target() {
        let mut context = connected();
        assert!(!command_available(by_id("window.moveLeft"), &context));
        context.can_move_tab_left = true;
        assert!(command_available(by_id("window.moveLeft"), &context));
        assert!(!command_available(by_id("session.moveLeft"), &context));
        context.can_move_session_up = true;
        assert!(command_available(by_id("session.moveLeft"), &context));
    }

    #[test]
    fn closes_app_tabs_while_disconnected_but_never_a_terminal_window() {
        let app_tab = CommandContext {
            can_mutate: false,
            has_tab: true,
            ..Default::default()
        };
        assert!(command_available(by_id("window.close"), &app_tab));
        let frozen_window = CommandContext {
            has_window: true,
            ..app_tab
        };
        assert!(!command_available(by_id("window.close"), &frozen_window));
    }

    #[test]
    fn marks_exactly_the_tmux_closes_for_confirmation() {
        let destructive: Vec<_> = COMMANDS
            .iter()
            .filter(|command| command.destructive)
            .map(|command| command.id)
            .collect();
        assert_eq!(destructive, ["session.close", "window.close", "pane.close"]);
    }

    #[test]
    fn a_saved_host_stays_deletable_while_its_server_is_unreachable() {
        let offline = CommandContext {
            has_host_profile: true,
            ..Default::default()
        };
        assert!(command_available(by_id("host.delete"), &offline));
        assert!(!by_id("host.delete").mutates);
    }

    #[test]
    fn a_row_command_is_available_only_while_published() {
        let mut context = connected();
        assert!(!command_available(by_id("git.stage"), &context));
        context.row_commands = vec!["git.stage"];
        assert!(command_available(by_id("git.stage"), &context));
        assert!(!command_available(by_id("git.unstage"), &context));
    }

    #[test]
    fn exactly_one_of_the_pinned_filters_applies() {
        let mut context = CommandContext::default();
        assert!(command_available(
            by_id("workspaces.showPinnedOnly"),
            &context
        ));
        assert!(!command_available(by_id("workspaces.showAll"), &context));
        context.pinned_only = true;
        assert!(!command_available(
            by_id("workspaces.showPinnedOnly"),
            &context
        ));
        assert!(command_available(by_id("workspaces.showAll"), &context));
    }

    #[test]
    fn resolves_a_positional_selector_to_its_index() {
        assert_eq!(
            selection_index("workspace.select4", "workspace.select"),
            Some(4)
        );
        assert_eq!(selection_index("tab.select9", "tab.select"), Some(9));
        assert_eq!(selection_index("tab.next", "tab.select"), None);
    }
}

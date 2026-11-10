//! The command registry, default keymaps, shortcut overrides from `app_state`,
//! the command palette, Quick Open and their dialogs. The app joins through
//! [`init`].
//!
//! # How a feature plugs in
//!
//! A command with no handler is shown unavailable, so a feature adds what it
//! implements and the palette follows:
//!
//! - [`on_command`] registers what a command does.
//! - [`update_context`] reports the state availability is computed from
//!   ([`registry::CommandContext`]).
//! - [`publish_row_commands`] offers the row commands for the row a surface holds.
//! - [`set_quick_open_source`] supplies Quick Open's workspaces, tabs and files.
//!
//! [`run`] is how anything other than a key press invokes a command.

pub mod dialogs;
pub mod fuzzy;
pub mod glyphs;
pub mod keymap;
pub mod palette;
pub mod quick_open;
pub mod registry;
pub mod shortcut_editor;

use std::{collections::HashMap, rc::Rc, time::Duration};

use desktop_core::{
    app_state::{load_app_state, update_app_state},
    platform,
};
use gpui_kit::{
    App, BorrowAppContext, Global, KeystrokeEvent, Window, base::Root, component::WindowExt,
};

use crate::{app::Core, notices, ui::shell::Shell};

use keymap::{
    ShortcutDisposition, ShortcutOverrides, repair_shortcut_collisions, shortcut_disposition,
    shortcut_from_keystroke,
};
use quick_open::QuickOpenSource;
use registry::{
    CommandContext, CommandDefinition, CommandId, Platform, Requires, command_available,
    command_by_id,
};

/// A context menu is the one overlay that is not a Kit dialog.
const POPUP_MENU_CONTEXT: &str = "PopupMenu";

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type RowRun = Rc<dyn Fn(CommandId, &mut Window, &mut App)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowSurface {
    Files,
    Git,
    Agents,
}

/// The actions the row a surface currently means can take now.
#[derive(Clone)]
pub struct RowCommandSource {
    pub available: Vec<CommandId>,
    pub run: RowRun,
}

pub struct Commands {
    platform: Platform,
    overrides: ShortcutOverrides,
    context: CommandContext,
    handlers: HashMap<CommandId, Handler>,
    row_sources: Vec<(RowSurface, RowCommandSource)>,
    quick_open: Option<Rc<dyn QuickOpenSource>>,
    /// Shown once the first window exists: which saved shortcuts repair removed.
    startup_notice: Option<String>,
}

impl Global for Commands {}

impl Commands {
    fn new(platform: Platform, overrides: ShortcutOverrides) -> Self {
        Self {
            platform,
            overrides,
            context: CommandContext::default(),
            handlers: HashMap::new(),
            row_sources: Vec::new(),
            quick_open: None,
            startup_notice: None,
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn overrides(&self) -> &ShortcutOverrides {
        &self.overrides
    }

    pub fn context(&self) -> &CommandContext {
        &self.context
    }

    /// A row command belongs to the surface that published it; anything else
    /// needs a feature to have registered it.
    pub fn is_available(&self, command: &CommandDefinition) -> bool {
        (command.requires == Some(Requires::Row) || self.handlers.contains_key(command.id))
            && command_available(command, &self.context)
    }

    fn row_source(&self, id: CommandId) -> Option<&RowCommandSource> {
        self.row_sources
            .iter()
            .map(|(_, source)| source)
            .find(|source| source.available.contains(&id))
    }

    fn publish_row_source(&mut self, surface: RowSurface, source: Option<RowCommandSource>) {
        let existing = self.row_sources.iter().position(|(s, _)| *s == surface);
        match (existing, source) {
            (Some(index), Some(source)) => self.row_sources[index].1 = source,
            (Some(index), None) => {
                self.row_sources.remove(index);
            }
            (None, Some(source)) => self.row_sources.push((surface, source)),
            (None, None) => {}
        }
        self.context.row_commands = self
            .row_sources
            .iter()
            .flat_map(|(_, source)| source.available.iter().copied())
            .collect();
    }
}

/// Registers what `id` does, making it available when its context allows.
pub fn on_command(id: CommandId, cx: &mut App, handler: impl Fn(&mut Window, &mut App) + 'static) {
    assert!(command_by_id(id).is_some(), "unknown command id {id}");
    cx.update_global(|commands: &mut Commands, _| {
        commands.handlers.insert(id, Rc::new(handler));
    });
}

pub fn update_context(cx: &mut App, update: impl FnOnce(&mut CommandContext)) {
    cx.update_global(|commands: &mut Commands, _| update(&mut commands.context));
}

/// Publishes a surface's current row, or `None` once the surface has no row.
pub fn publish_row_commands(surface: RowSurface, source: Option<RowCommandSource>, cx: &mut App) {
    cx.update_global(|commands: &mut Commands, _| commands.publish_row_source(surface, source));
}

pub fn set_quick_open_source(source: Rc<dyn QuickOpenSource>, cx: &mut App) {
    cx.update_global(|commands: &mut Commands, _| commands.quick_open = Some(source));
}

/// Runs a command's handler. Row commands go to the surface that published them.
/// Callers offer only what [`Commands::is_available`] allows.
pub fn run(id: CommandId, window: &mut Window, cx: &mut App) {
    let Some(command) = command_by_id(id) else {
        return;
    };
    let commands = cx.global::<Commands>();
    if command.requires == Some(Requires::Row) {
        // The row may have gone between the palette opening and Enter, and that
        // has to be said rather than silently doing nothing.
        let Some(source) = commands.row_source(id) else {
            let title = command.title.trim_end_matches('…');
            notices::notify(
                &format!("{title} is unavailable: nothing is selected in that panel any more."),
                window,
                cx,
            );
            return;
        };
        let run_row = source.run.clone();
        run_row(id, window, cx);
        return;
    }
    if let Some(handler) = commands.handlers.get(id).cloned() {
        handler(window, cx);
    }
}

/// How often a pending save is checked for its result.
const SAVE_POLL: Duration = Duration::from_millis(50);

/// Saves the overrides, reports a failure, and applies them at once.
pub fn set_overrides(overrides: ShortcutOverrides, window: &mut Window, cx: &mut App) {
    let save = store_overrides(&overrides, cx);
    cx.update_global(|commands: &mut Commands, _| commands.overrides = overrides);
    window
        .spawn(cx, async move |cx| {
            // Polled, not awaited: GPUI's test scheduler rejects a wake from a
            // runtime thread.
            while !save.is_finished() {
                cx.background_executor().timer(SAVE_POLL).await;
            }
            let failure = match save.await {
                Ok(Ok(())) => return,
                Ok(Err(failure)) => failure,
                Err(error) => error.to_string(),
            };
            cx.update(|window, cx| {
                notices::notify(&format!("Could not save shortcuts: {failure}"), window, cx);
            })
            .ok();
        })
        .detach();
}

/// Starts the save now, on the backend's runtime; the result is optional.
fn store_overrides(
    overrides: &ShortcutOverrides,
    cx: &App,
) -> tokio::task::JoinHandle<Result<(), String>> {
    let store = cx.global::<Core>().app_state.clone();
    let overrides = overrides.clone();
    platform::spawn(async move {
        update_app_state(&store, move |state| {
            state.commands.shortcut_overrides = overrides;
        })
        .await
    })
}

fn on_keystroke(event: &KeystrokeEvent, window: &mut Window, cx: &mut App) {
    // A window with no Kit root (some tests) has no overlays or inputs to consult.
    if window.root::<Root>().flatten().is_none() {
        return;
    }
    // A key press inside an IME composition belongs to the composition, in a
    // Kit input or in a terminal pane.
    let focused_input = window
        .focused_input(cx)
        .and_then(|input| input.as_input().cloned());
    if focused_input.is_some_and(|input| dialogs::is_composing(&input, window, cx))
        || crate::terminal::input::is_composing(window, cx)
    {
        return;
    }
    let overlay_open = window.has_active_dialog(cx)
        || event
            .context_stack
            .iter()
            .any(|context| context.contains(POPUP_MENU_CONTEXT));
    let text_field = window.has_focused_input(cx);
    let pressed = shortcut_from_keystroke(&event.keystroke);
    let commands = cx.global::<Commands>();
    let disposition = shortcut_disposition(
        &pressed,
        overlay_open,
        text_field,
        commands.platform,
        &commands.overrides,
        commands.context.has_terminal_selection,
        |command| commands.is_available(command),
    );
    match disposition {
        ShortcutDisposition::Ignore | ShortcutDisposition::Yield(_) => {}
        ShortcutDisposition::Swallow => cx.stop_propagation(),
        ShortcutDisposition::Run(id) => {
            run(id, window, cx);
            cx.stop_propagation();
        }
    }
}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    let platform = Platform::current();
    let saved = load_app_state(&cx.global::<Core>().app_state);
    let mut overrides = saved
        .map(|state| state.commands.shortcut_overrides)
        .unwrap_or_default();
    let displaced = repair_shortcut_collisions(platform, &mut overrides);
    let mut commands = Commands::new(platform, overrides);
    if !displaced.is_empty() {
        let list: Vec<String> = displaced
            .iter()
            .map(|(id, shortcut)| format!("{id} ({shortcut})"))
            .collect();
        commands.startup_notice = Some(format!(
            "Disabled conflicting saved shortcuts: {}.",
            list.join(", ")
        ));
    }
    let repaired = (!displaced.is_empty()).then(|| commands.overrides.clone());
    cx.set_global(commands);
    if let Some(repaired) = repaired {
        // Without a window yet, there is nowhere to report a failed save; the
        // repair simply runs again on the next launch.
        drop(store_overrides(&repaired, cx));
    }

    on_command("commands.show", cx, palette::open);
    on_command("workspaces.switch", cx, quick_open::open);
    on_command("shortcuts.configure", cx, shortcut_editor::open);

    cx.intercept_keystrokes(on_keystroke).detach();
    cx.observe_new::<Shell>(|_, window, cx| {
        let Some(window) = window else {
            return;
        };
        let notice = cx.update_global(|commands: &mut Commands, _| commands.startup_notice.take());
        if let Some(notice) = notice {
            window.defer(cx, move |window, cx| {
                notices::notify(&notice, window, cx);
            });
        }
    })
    .detach();
}

//! Muxflow's native desktop app, built on GPUI.
//!
//! # Module map
//!
//! Each feature owns its module. A feature joins the app through one line in
//! [`features::init`], so features do not meet in the app root.
//!
//! | Module | What lives there |
//! |---|---|
//! | [`app`] | Boot: application, [`app::Core`], the main window |
//! | [`features`] | The single feature-registration point |
//! | [`theme`] | Generated palette, metrics, light/dark, UI scale |
//! | [`fonts`] | Embedded fonts |
//! | [`paths`] | [`paths::NativePaths`], the same directories Tauri uses |
//! | [`ui`] | Window shell and title bar; the accessibility baseline |
//! | [`ui::chrome`] | Window body: dock, sidebar, tabs, right panel |
//! | [`commands`] | Command registry, keymap, palette, Quick Open |
//! | [`connection_ui`] | Connection, host profile and setup flows |
//! | [`settings`] | Live preferences and the Settings dialog |
//! | [`diagnostics`] | Incident record format, the copy-chord journal |
//! | [`menus`] | Application menu and its accelerators |
//! | [`context_menu`] | Context menu items on Kit's popup |
//! | [`notices`] | Which status messages show, as notifications |
//! | [`tooltips`] | A control's name with its shortcut |
//! | [`window_state`] | Window geometry, restored and saved |
//! | [`terminal`] | Terminal panes: engine link, element, sizing, and the input, reading and integration seams |
//! | [`agents`] | Agents list, agent status, notifications, sounds and host hook setup |
//! | [`automations`] | The host's scheduled agent runs, as a right-panel segment |
//! | [`dragdrop`] | Dragging tabs, documents and panes onto tabs, workspaces and pane edges |
//! | [`editor`] | File tabs on the Kit code editor: read, autosave, find, grammars; document-tab surface routing |
//! | [`git`] | Source Control panel and Git diff tabs |
//! | [`ports`] | Port forwarding from SSH hosts, as a right-panel segment |
//! | [`transfers`] | The transfer vocabulary uploads and downloads share |
//!
//! Colors and metrics come from [`theme`]. Controls follow the accessibility
//! baseline in [`ui`].

pub mod agents;
pub mod app;
pub mod automations;
pub mod commands;
pub mod connection_ui;
pub mod context_menu;
pub mod diagnostics;
pub mod dragdrop;
pub mod editor;
pub mod features;
pub mod fonts;
pub mod git;
pub mod menus;
pub mod notices;
pub mod paths;
pub mod ports;
pub mod settings;
pub mod terminal;
pub mod theme;
pub mod tooltips;
pub mod transfers;
pub mod ui;
pub mod window_state;
pub mod workspaces;

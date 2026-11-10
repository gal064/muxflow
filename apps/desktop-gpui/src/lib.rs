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
//! | [`menus`] | Application menu and its accelerators |
//! | [`context_menu`] | Context menu items on Kit's popup |
//! | [`notices`] | Which status messages show, as notifications |
//! | [`tooltips`] | A control's name with its shortcut |
//! | [`window_state`] | Window geometry, restored and saved |
//!
//! Colors and metrics come from [`theme`]. Controls follow the accessibility
//! baseline in [`ui`].

pub mod app;
pub mod commands;
pub mod connection_ui;
pub mod context_menu;
pub mod features;
pub mod fonts;
pub mod menus;
pub mod notices;
pub mod paths;
pub mod theme;
pub mod tooltips;
pub mod ui;
pub mod window_state;

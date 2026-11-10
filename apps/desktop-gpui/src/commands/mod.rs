//! Owned by T2.3 `commands-keymap`: the command registry, default keymaps,
//! shortcut overrides from `app_state`, the command palette and Quick Open.
//! Everything goes under `commands/`; the app joins through [`init`].

use gpui_kit::App;

/// Called once from [`crate::features::init`].
pub fn init(_cx: &mut App) {}

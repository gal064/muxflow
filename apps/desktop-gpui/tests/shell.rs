//! The window shell through GPUI's test platform: saved appearance, UI scale,
//! and the minimum size.

use std::path::{Path, PathBuf};

use desktop_core::app_state::ThemePreference;
use gpui_kit::{App, TestAppContext, component::Theme, px};
use muxflow_gpui::{
    app::{self, Core},
    theme::{self, Appearance},
    ui::shell::Shell,
};

const NO_STATE: &str = "/nonexistent/app-state.json";

fn boot(state: &Path, cx: &mut App) {
    gpui_kit::init(cx);
    app::init(Core::at(state), cx);
}

fn write_state(dir: &Path, theme: &str, ui_scale: f64) -> PathBuf {
    let path = dir.join("app-state.json");
    let state = serde_json::json!({
        "schemaVersion": 1,
        "appTabs": [],
        "workspaceUi": [],
        "shell": { "theme": theme, "uiScale": ui_scale },
    });
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    path
}

#[gpui_kit::test]
fn saved_theme_and_scale_reach_the_window(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), "light", 1.2);
    cx.update(|cx| boot(&path, cx));
    let (_, window_cx) = cx.add_window_view(Shell::new);
    window_cx.update(|_, cx| {
        let appearance = cx.global::<Appearance>();
        assert_eq!(appearance.preference, ThemePreference::Light);
        assert!(!appearance.is_dark);
        assert_eq!(appearance.scale, 1.2);
        assert_eq!(appearance.palette, theme::light());
        let theme = Theme::global(cx);
        assert_eq!(theme.font_size, px(19.2));
        assert_eq!(theme.title_bar, theme::light().chrome_bg.into());
        assert_eq!(theme.primary, theme::light().accent.into());
    });
}

#[gpui_kit::test]
fn the_minimum_size_is_tauris(cx: &mut TestAppContext) {
    let options = cx.update(|cx| {
        boot(Path::new(NO_STATE), cx);
        app::main_window_options(cx)
    });
    let min = options.window_min_size.expect("a minimum size");
    assert_eq!((min.width, min.height), (px(320.), px(240.)));
}

//! Boot: the application, the backend handle, the main window.

use std::{path::Path, sync::Arc};

use desktop_core::{app_state::AppStateStore, platform::AppPaths};
use gpui_kit::{
    AnyWindowHandle, App, Bounds, Global, QuitMode, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions, point, px, size,
};

use crate::{features, fonts, paths::NativePaths, theme, ui::shell::Shell, window_state};

/// The app's minimum window size, as Tauri's `minWidth` and `minHeight`.
pub const MIN_WINDOW_WIDTH: f32 = 320.;
pub const MIN_WINDOW_HEIGHT: f32 = 240.;

/// The backend handle features reach through `cx.global::<Core>()`.
pub struct Core {
    pub paths: NativePaths,
    pub app_state: Arc<AppStateStore>,
}

impl Global for Core {}

impl Core {
    pub fn load() -> Self {
        let config_dir = NativePaths
            .app_config_dir()
            .expect("resolve the config directory");
        Self::at(&config_dir.join("app-state.json"))
    }

    /// The saved state at `path`, as Tauri's `app-state.json`.
    pub fn at(path: &Path) -> Self {
        Self {
            paths: NativePaths,
            app_state: Arc::new(AppStateStore::load(path.to_owned())),
        }
    }
}

pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            gpui_kit::init(cx);
            fonts::register(cx);
            init(Core::load(), cx);
            crate::connection_ui::start(cx).expect("load the saved host profiles");
            open_main_window(cx).expect("open the main window");
            cx.activate(true);
        });
}

/// Everything before the first window, after `gpui_kit::init` and
/// `fonts::register`. Those two stay with the caller because GPUI's test
/// platform has no real text system to register fonts with.
pub fn init(core: Core, cx: &mut App) {
    let (theme, scale) = (core.app_state.theme(), core.app_state.ui_scale());
    cx.set_global(core);
    let system = cx.window_appearance();
    theme::apply(theme, scale, system, cx);
    features::init(cx);
}

pub fn main_window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(window_state::saved_bounds(cx).unwrap_or_else(|| {
            WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(800.)), cx))
        })),
        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
        titlebar: Some(TitlebarOptions {
            title: Some("Muxflow".into()),
            appears_transparent: true,
            // Tauri's `trafficLightPosition`. GPUI centers the 14px lights in a
            // container of height `button + 2y`, so they sit at y + 7 from the top
            // of the 38px bar.
            traffic_light_position: Some(point(px(12.), px(13.))),
        }),
        app_owns_titlebar_drag: true,
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some("dev.muxflow.desktop".to_owned()),
        ..Default::default()
    }
}

pub fn open_main_window(cx: &mut App) -> gpui_kit::Result<AnyWindowHandle> {
    let options = main_window_options(cx);
    let (window, _) = gpui_kit::open_window(options, cx, Shell::open)?;
    Ok(window)
}

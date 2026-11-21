//! Boot: the application, the backend handle, the main window.

use std::{borrow::Cow, path::Path, sync::Arc};

use desktop_core::{app_state::AppStateStore, platform::AppPaths};
use gpui_kit::{
    AnyWindowHandle, App, AssetSource, Bounds, Global, QuitMode, SharedString, TitlebarOptions,
    WindowBounds, WindowDecorations, WindowOptions, point, px, size,
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

// The Lucide icons the app draws beyond the Kit's 101 defaults (listed in
// `gpui-kit-assets`' `default-icons.txt`), by their `IconName` variant. An
// icon that is neither here nor a default draws nothing, so add it here in
// the change that first draws it. Only the listed SVGs are embedded.
gpui_kit::assets::icon_assets!(
    pub ExtraIcons,
    [
        // The chat's details card.
        Server,
        GitBranch,
        GitCommitHorizontal,
        FileDiff,
        // Message buttons and fork.
        GitFork,
        Pencil,
        // Tool rows.
        FilePenLine,
        Wrench,
        // Thinking.
        Brain,
        // Attachments.
        Paperclip,
        X,
        Image,
        // Background tasks and compacting.
        Layers,
        Shrink,
        // The Agents header's History button.
        Clock,
    ]
);

/// The Kit's default icons with [`ExtraIcons`] beside them.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

pub fn run() {
    crate::update::wait_for_predecessor();
    gpui_kit::application()
        .with_assets(AppAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            gpui_kit::init(cx);
            fonts::register(cx);
            init(Core::load(), cx);
            crate::connection_ui::start(cx).expect("load the saved host profiles");
            crate::update::start(cx);
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

#[cfg(test)]
mod tests {
    use gpui_kit::assets::{Assets, IconName};

    use super::*;

    /// Every extra icon loads, none of them is one the Kit already embeds,
    /// and the Kit's own still load beside them.
    #[test]
    fn the_app_s_assets_carry_the_extra_icons_beside_the_kit_s() {
        let extras = ExtraIcons.list("icons/").unwrap();
        let listed_above = [
            IconName::Server,
            IconName::GitBranch,
            IconName::GitCommitHorizontal,
            IconName::FileDiff,
            IconName::GitFork,
            IconName::Pencil,
            IconName::FilePenLine,
            IconName::Wrench,
            IconName::Brain,
            IconName::Paperclip,
            IconName::X,
            IconName::Image,
            IconName::Layers,
            IconName::Shrink,
            IconName::Clock,
        ];
        assert_eq!(extras.len(), listed_above.len());
        for name in listed_above {
            assert!(extras.contains(&name.path()), "{} is embedded", name.path());
        }
        for path in &extras {
            // The Kit's source refuses a path it does not embed.
            assert!(
                !matches!(Assets.load(path), Ok(Some(_))),
                "{path} is a default"
            );
            let svg = AppAssets.load(path).unwrap().expect(path);
            assert!(svg.starts_with(b"<svg"), "{path}");
        }
        let default = IconName::ChevronDown.path();
        assert!(AppAssets.load(&default).unwrap().is_some());
        let listed = AppAssets.list("icons/").unwrap();
        assert!(listed.contains(&default) && listed.contains(&IconName::Server.path()));
        assert!(!matches!(
            AppAssets.load("icons/not-an-icon.svg"),
            Ok(Some(_))
        ));
    }
}

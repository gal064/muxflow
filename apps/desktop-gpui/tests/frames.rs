//! Real offscreen renders of the window: light and dark at UI scale 1.0 and
//! 1.2, at the default and the minimum size, with the embedded fonts. Set
//! `MUXFLOW_GPUI_FRAME_DIR` to keep the PNGs. The renders are Linux only,
//! because hosted macOS has no reliable offscreen GPU path; the font check runs
//! everywhere.

fn main() {
    weights_are_distinct_faces();
    #[cfg(target_os = "linux")]
    frames::run();
}

/// The static Inter faces load as separate fonts. GPUI's macOS text system
/// silently drops faces that share a PostScript name, so this runs there too.
fn weights_are_distinct_faces() {
    use gpui_kit::{FontWeight, HeadlessAppContext, assets::Assets, font};
    use muxflow_gpui::fonts;
    use std::sync::Arc;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| fonts::register(cx));
    let text = cx.text_system().clone();
    let face = |weight| {
        let mut sans = font(fonts::SANS);
        sans.weight = weight;
        text.resolve_font(&sans)
    };
    let faces = [
        face(FontWeight::NORMAL),
        face(FontWeight::MEDIUM),
        face(FontWeight::SEMIBOLD),
        face(FontWeight::BOLD),
    ];
    for (index, id) in faces.iter().enumerate() {
        assert!(
            !faces[index + 1..].contains(id),
            "two Inter weights resolved to one face: {faces:?}"
        );
    }
}

#[cfg(target_os = "linux")]
mod frames {
    use std::{collections::HashSet, path::Path, sync::Arc};

    use gpui_kit::{
        AnyWindowHandle, Bounds, HeadlessAppContext, Rgba, WindowBounds, assets::Assets, px, size,
    };
    use muxflow_gpui::{
        app::{self, Core},
        fonts,
        theme::{self, Palette},
        ui::shell::Shell,
    };

    fn context(state_path: &Path) -> HeadlessAppContext {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            fonts::register(cx);
            app::init(Core::at(state_path), cx);
        });
        cx
    }

    fn write_state(dir: &Path, theme: &str, ui_scale: f64) -> std::path::PathBuf {
        let path = dir.join("app-state.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "appTabs": [],
                "workspaceUi": [],
                "shell": { "theme": theme, "uiScale": ui_scale },
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }

    fn open(cx: &mut HeadlessAppContext, width: f32, height: f32) -> AnyWindowHandle {
        cx.update(|cx| {
            let mut options = app::main_window_options(cx);
            options.window_bounds = Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: size(px(width), px(height)),
            }));
            options.focus = false;
            options.show = false;
            gpui_kit::open_window(options, cx, Shell::open)
                .expect("open the headless window")
                .0
        })
    }

    fn rgb(color: Rgba) -> [u8; 3] {
        let c = |v: f32| (v * 255.).round() as u8;
        [c(color.r), c(color.g), c(color.b)]
    }

    fn render_case(theme: &str, scale: f64, width: u32, height: u32, palette: &Palette) {
        let dir = tempfile::tempdir().unwrap();
        let state = write_state(dir.path(), theme, scale);
        let mut cx = context(&state);
        let handle = open(&mut cx, width as f32, height as f32);
        let frame = cx
            .capture_screenshot(handle)
            .expect("offscreen rendering is unavailable (no Vulkan driver?)");
        let label = format!("{theme}-{scale}-{width}x{height}");

        if let Ok(out) = std::env::var("MUXFLOW_GPUI_FRAME_DIR") {
            std::fs::create_dir_all(&out).unwrap();
            frame
                .save(format!("{out}/{label}.png"))
                .expect("save the frame");
        }

        // The headless renderer draws at 2x, so logical points are doubled here.
        assert_eq!(frame.dimensions(), (width * 2, height * 2), "{label}");
        let at = |x: u32, y: u32| {
            let p = frame.get_pixel(x * 2, y * 2).0;
            [p[0], p[1], p[2]]
        };
        // The window is themed: the body below the title bar is the chrome ground.
        assert_eq!(
            at(width / 2, height - 8),
            rgb(palette.chrome_bg),
            "{label} body"
        );
        // The title bar's hairline sits at its scaled height.
        let line = (28.0 * scale).round() as u32;
        let hairline = rgb(palette.chrome_hairline);
        assert!(
            (line.saturating_sub(2)..=line + 1).any(|y| at(width / 2, y) == hairline),
            "{label}: no hairline near y={line}"
        );
        // Text was shaped with real fonts: anti-aliasing gives many colors.
        let colors: HashSet<[u8; 4]> = frame.pixels().map(|p| p.0).collect();
        assert!(
            colors.len() >= 16,
            "{label}: frame looks blank ({} colors)",
            colors.len()
        );
        println!("{label}: ok, {} colors", colors.len());
    }

    pub fn run() {
        for (theme, palette) in [("dark", theme::dark()), ("light", theme::light())] {
            for scale in [1.0, 1.2] {
                render_case(theme, scale, 1280, 800, &palette);
                render_case(theme, scale, 320, 240, &palette);
            }
        }
    }
}

//! Real offscreen renders of the window: light and dark at UI scale 1.0 and
//! 1.2, at the minimum, default and a large size, with the embedded fonts. Set
//! `MUXFLOW_GPUI_FRAME_DIR` to keep the PNGs. The renders are Linux only,
//! because hosted macOS has no reliable offscreen GPU path; the font check runs
//! everywhere.

fn main() {
    weights_are_distinct_faces();
    terminal_styles_are_distinct_faces();
    #[cfg(target_os = "linux")]
    frames::run();
}

/// The static Inter faces load as separate fonts. GPUI's macOS text system
/// silently drops faces that share a PostScript name, so this runs there too.
fn weights_are_distinct_faces() {
    use gpui_kit::{FontWeight, HeadlessAppContext, assets::Assets, font};
    use muxflow::fonts;
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
    use muxflow::{
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
        let tab = |id: &str, title: &str, order: u32| {
            serde_json::json!({
                "id": id, "hostProfileId": "local", "serverIdentity": "tmux",
                "sessionId": "$1", "sessionName": "main", "kind": "file",
                "resource": format!("/repo/{title}"), "title": title, "order": order,
            })
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "appTabs": [
                    tab("a", "src/main.rs", 0),
                    tab("b", "README.md", 1),
                ],
                "workspaceUi": [{
                    "hostProfileId": "local", "serverIdentity": "tmux",
                    "sessionId": "$1", "sessionName": "main",
                    "selectedAppTabId": "a",
                    "dockedAppTabId": "b", "dockSide": "right",
                }],
                "shell": { "theme": theme, "uiScale": ui_scale, "panelOpen": true },
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
        // The window is themed: the work area below the title bar is the terminal ground.
        assert_eq!(
            at(width / 2, height - 8),
            rgb(palette.term_bg),
            "{label} body"
        );
        // The title bar's hairline sits at its scaled height: with tabs in the
        // bar, the height of the tab strip it replaces.
        let line = (34.0 * scale).round() as u32;
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

    /// A window with one row that has a context menu, to draw the menu on its own.
    struct Demo;

    impl gpui_kit::Render for Demo {
        fn render(
            &mut self,
            window: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            use gpui_kit::{
                InteractiveElement, ParentElement, Styled, TestSupportExt,
                component::menu::ContextMenuExt, div,
            };
            use muxflow::{
                context_menu::{self, Entry, Item},
                theme::palette,
            };
            let palette = palette(cx);
            let entries = std::rc::Rc::new(vec![
                Entry::from(
                    Item::new("Open", |_, _| {}).hint(&muxflow::menus::RunCommand {
                        id: "commands.show",
                    }),
                ),
                Entry::from(Item::new("Rename…", |_, _| {}).disabled(true)),
                Entry::Separator,
                Entry::from(Item::new("Sort by name", |_, _| {}).checked(true)),
                Entry::from(Item::new("Sort by date", |_, _| {}).checked(false)),
                Entry::Separator,
                Entry::from(Item::new("Delete…", |_, _| {}).destructive()),
            ]);
            let tooltip = gpui_kit::component::tooltip::Tooltip::new("Toggle sidebar (Ctrl+B)")
                .build(window, cx);
            div()
                .size_full()
                .bg(palette.chrome_bg)
                .p(gpui_kit::px(24.))
                .child(
                    div()
                        .id("row")
                        .w(gpui_kit::px(220.))
                        .h(gpui_kit::px(28.))
                        .text_color(palette.chrome_ink)
                        .child("src/main.rs")
                        .test_support()
                        .context_menu(move |menu, window, cx| {
                            context_menu::fill(menu, &entries, window, cx)
                        }),
                )
                // How a tooltip looks: the card the app builds on hover.
                .child(div().mt(gpui_kit::px(160.)).child(tooltip))
        }
    }

    // Macros, because the frame type belongs to a crate this one does not name.
    macro_rules! save {
        ($frame:expr, $label:expr) => {
            if let Ok(out) = std::env::var("MUXFLOW_GPUI_FRAME_DIR") {
                std::fs::create_dir_all(&out).unwrap();
                $frame
                    .save(format!("{out}/{}.png", $label))
                    .expect("save the frame");
            }
        };
    }

    macro_rules! differing_pixels {
        ($a:expr, $b:expr) => {
            $a.pixels().zip($b.pixels()).filter(|(a, b)| a != b).count()
        };
    }

    /// Menus, notices, tooltips and context menus, each against the same window
    /// without them, so a surface that never drew fails here.
    fn render_surfaces(theme: &str) {
        use gpui_kit::{
            component::WindowExt as _,
            test::{TestQueryExt, TestWindowExt},
        };
        use muxflow::notices;

        let dir = tempfile::tempdir().unwrap();
        let state = write_state(dir.path(), theme, 1.0);
        let mut cx = context(&state);
        let handle = open(&mut cx, 1000., 400.);
        cx.update_window(handle, |_, window, _| window.activate_window())
            .unwrap();
        let rest = cx.capture_screenshot(handle).expect("offscreen rendering");
        save!(rest, format!("surface-{theme}-rest"));

        let shoot = |cx: &mut HeadlessAppContext, name: &str| {
            // Past the animations that bring a surface in.
            cx.advance_clock(std::time::Duration::from_millis(400));
            cx.run_until_parked();
            // The screenshot is of the last frame drawn, and a timer does not draw one.
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
            let frame = cx.capture_screenshot(handle).expect("offscreen rendering");
            save!(frame, format!("surface-{theme}-{name}"));
            let changed = differing_pixels!(rest, frame);
            assert!(
                changed > 400,
                "{theme} {name}: nothing was drawn ({changed})"
            );
        };

        // The main menu, with its View submenu open.
        cx.update_window(handle, |_, window, cx| window.click("app-menu", cx))
            .unwrap();
        shoot(&mut cx, "menu");
        cx.update_window(handle, |_, window, cx| {
            let id = window.find_by_label("View").path().last().cloned().unwrap();
            window.hover(id, cx)
        })
        .unwrap();
        shoot(&mut cx, "menu-view");
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();
        cx.update_window(handle, |_, window, cx| window.press("escape", cx))
            .unwrap();

        // An information notice, then a problem that replaces it.
        cx.update_window(handle, |_, window, cx| {
            notices::notify("Copied the path to the clipboard", window, cx)
        })
        .unwrap();
        shoot(&mut cx, "notice");
        cx.update_window(handle, |_, window, cx| {
            notices::notify("Could not save shortcuts: the disk is full", window, cx)
        })
        .unwrap();
        shoot(&mut cx, "notice-problem");
        cx.update_window(handle, |_, window, cx| window.clear_notifications(cx))
            .unwrap();
    }

    fn render_context_menu(theme: &str) {
        use gpui_kit::{AppContext, test::TestWindowExt};

        let dir = tempfile::tempdir().unwrap();
        let state = write_state(dir.path(), theme, 1.0);
        let mut cx = context(&state);
        let handle = cx.update(|cx| {
            let mut options = app::main_window_options(cx);
            options.window_bounds = Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: size(px(420.), px(300.)),
            }));
            options.focus = false;
            options.show = false;
            gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Demo))
                .expect("open the headless window")
                .0
        });
        let rest = cx.capture_screenshot(handle).expect("offscreen rendering");
        cx.update_window(handle, |_, window, cx| window.right_click("row", cx))
            .unwrap();
        cx.run_until_parked();
        let frame = cx.capture_screenshot(handle).expect("offscreen rendering");
        save!(frame, format!("surface-{theme}-context-menu"));
        assert!(
            differing_pixels!(rest, frame) > 400,
            "{theme}: the context menu did not draw"
        );
    }

    pub fn run() {
        for (theme, palette) in [("dark", theme::dark()), ("light", theme::light())] {
            for scale in [1.0, 1.2] {
                render_case(theme, scale, 1920, 1200, &palette);
                render_case(theme, scale, 1280, 800, &palette);
                render_case(theme, scale, 320, 240, &palette);
            }
            render_surfaces(theme);
            render_context_menu(theme);
        }
    }
}

/// Port of `theme.test.ts` "names every style the atlas can hold": the
/// terminal's four styles each resolve to their own bundled face, so bold and
/// italic are never synthesized from the regular one.
fn terminal_styles_are_distinct_faces() {
    use gpui_kit::{FontStyle, FontWeight, HeadlessAppContext, assets::Assets, font};
    use muxflow::fonts;
    use std::sync::Arc;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| fonts::register(cx));
    let text = cx.text_system().clone();
    let face = |weight, style| {
        let mut mono = font(fonts::MONO);
        mono.weight = weight;
        mono.style = style;
        text.resolve_font(&mono)
    };
    let faces = [
        face(FontWeight::NORMAL, FontStyle::Normal),
        face(FontWeight::BOLD, FontStyle::Normal),
        face(FontWeight::NORMAL, FontStyle::Italic),
        face(FontWeight::BOLD, FontStyle::Italic),
    ];
    for (index, id) in faces.iter().enumerate() {
        assert!(
            !faces[index + 1..].contains(id),
            "two terminal styles resolved to one face: {faces:?}"
        );
    }
}

use std::borrow::Cow;
use std::path::PathBuf;

use gpui_kit::*;
use spike_markdown::kit_probe::KitProbe;
use spike_markdown::view::{LiveEditor, bind_keys};

const BOLD: &[u8] = include_bytes!("../assets/fonts/Inter-Bold.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/Inter-BoldItalic.ttf");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut fixture = None;
    let mut out = None;
    let mut state = None;
    let mut probe = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--out" => out = iter.next().map(PathBuf::from),
            "--state-log" => state = iter.next().map(PathBuf::from),
            "--probe-kit" => probe = true,
            other => fixture = Some(PathBuf::from(other)),
        }
    }
    let fixture = fixture.expect("usage: spike-markdown <file> [--out path] [--state-log path] [--probe-kit]");
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{}.out", fixture.display())));
    let source = std::fs::read_to_string(&fixture).expect("read fixture");

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        spike_shared::fonts::register(cx);
        // GPUI's Linux text system does not apply the variable font's weight
        // axis, so bold needs static instances of the same family (REPORT.md).
        cx.text_system()
            .add_fonts(vec![Cow::Borrowed(BOLD), Cow::Borrowed(BOLD_ITALIC)])
            .expect("add static bold faces");
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        bind_keys(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1200.), px(980.)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Muxflow live Markdown spike".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        if probe {
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| KitProbe::new(source, window, cx)))
                .expect("open window");
        } else {
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| LiveEditor::new(source, out, state, window, cx)))
                .expect("open window");
        }
        cx.activate(true);
    });
}

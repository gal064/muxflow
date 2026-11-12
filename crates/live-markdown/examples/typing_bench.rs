//! Typing latency and frame time of the Live view on a large file, to set the
//! size limit. Build it in release and run it where a Vulkan driver is
//! available (the QA sandbox's lavapipe is enough; nothing is rasterized
//! unless `--shot` asks for one):
//!
//!   typing_bench <file.md> [--fonts <dir>] [--keys <n>] [--shot <out.png>]
//!
//! It opens the view in a headless window with the platform's real text
//! system, puts the caret in the middle of the document, and for each key
//! times the edit through the platform input handler and the frame that
//! follows (layout, prepaint and paint). Prints p50, p95 and max of each.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{
    AnyWindowHandle, AppContext, EntityInputHandler, Focusable, HeadlessAppContext, Hsla, Window,
    assets::Assets, hsla, px, size,
};
use live_markdown::view::{LiveStyle, LiveView};

fn style() -> LiveStyle {
    let gray = |l: f32| hsla(0., 0., l, 1.);
    LiveStyle {
        sans: "Inter Variable".into(),
        mono: "JetBrains Mono".into(),
        background: gray(0.1),
        ink: gray(0.85),
        ink_strong: gray(0.95),
        dim: gray(0.6),
        accent: hsla(0.6, 0.7, 0.6, 1.),
        selection: Hsla {
            a: 0.3,
            ..hsla(0.6, 0.7, 0.6, 1.)
        },
        raised: gray(0.15),
        hairline: gray(0.2),
        border: gray(0.25),
        dark: true,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn report(name: &str, mut ms: Vec<f64>) {
    ms.sort_by(f64::total_cmp);
    println!(
        "{name:>6}: p50 {:7.2} ms  p95 {:7.2} ms  max {:7.2} ms",
        percentile(&ms, 0.5),
        percentile(&ms, 0.95),
        ms[ms.len() - 1]
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut file = None;
    let mut fonts = None;
    let mut keys = 60usize;
    let mut shot = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fonts" => fonts = args.next().map(PathBuf::from),
            "--keys" => {
                keys = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("--keys <n>")
            }
            "--shot" => shot = args.next().map(PathBuf::from),
            other => file = Some(PathBuf::from(other)),
        }
    }
    let text = std::fs::read_to_string(
        file.expect("usage: typing_bench <file.md> [--fonts dir] [--keys n] [--shot png]"),
    )
    .expect("read the file");
    {
        use live_markdown::projection::{Active, parse, project};
        let start = Instant::now();
        let ast = parse(&text);
        let parsed = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let projection = project(&text, &ast, &Active::default());
        println!(
            "parse {parsed:.1} ms, project {:.1} ms ({} blocks, {} leaves)",
            start.elapsed().as_secs_f64() * 1000.,
            projection.blocks.len(),
            projection.leaves.len()
        );
    }
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        live_markdown::view::init(cx);
        if let Some(dir) = &fonts {
            let faces = std::fs::read_dir(dir)
                .expect("read the fonts directory")
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "ttf"))
                .map(|path| Cow::Owned(std::fs::read(path).expect("read a font")))
                .collect();
            cx.text_system().add_fonts(faces).expect("add the fonts");
        }
    });
    let opened = Instant::now();
    let mut view = None;
    let window = cx
        .open_window(size(px(1600.), px(1000.)), |window, cx| {
            let made = cx.new(|cx| LiveView::new(&text, style(), window, cx));
            view = Some(made.clone());
            made
        })
        .expect("open a window");
    let handle: AnyWindowHandle = window.into();
    let view = view.expect("the view");
    let draw = |cx: &mut HeadlessAppContext| {
        let start = Instant::now();
        cx.update_window(handle, |_, window: &mut Window, cx| {
            window.draw(cx).clear(cx)
        })
        .expect("draw");
        start.elapsed().as_secs_f64() * 1000.
    };
    cx.update_window(handle, |_, window, cx| {
        window.focus(&view.read(cx).focus_handle(cx), cx)
    })
    .expect("focus");
    let first = draw(&mut cx);
    println!(
        "{} bytes: open and first frame {:.0} ms (first frame {first:.0} ms)",
        text.len(),
        opened.elapsed().as_secs_f64() * 1000.
    );
    // The middle of the document, at the end of a line of text.
    let middle = text[..text.len() / 2].rfind('\n').unwrap_or(0);
    let at = text[..middle].encode_utf16().count();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.replace_text_in_range(Some(at..at), "", window, cx)
        })
    })
    .expect("place the caret");
    draw(&mut cx);
    let (mut edits, mut frames, mut totals) = (Vec::new(), Vec::new(), Vec::new());
    for key in 0..keys {
        let typed = if key % 6 == 5 { " " } else { "x" };
        let start = Instant::now();
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.replace_text_in_range(None, typed, window, cx)
            })
        })
        .expect("type");
        let edit = start.elapsed().as_secs_f64() * 1000.;
        let frame = draw(&mut cx);
        edits.push(edit);
        frames.push(frame);
        totals.push(edit + frame);
    }
    report("edit", edits);
    report("frame", frames);
    report("total", totals);
    let mut moves = Vec::new();
    for _ in 0..keys {
        let start = Instant::now();
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_action(Box::new(live_markdown::view::MoveLeft), cx)
        })
        .expect("move");
        moves.push(start.elapsed().as_secs_f64() * 1000. + draw(&mut cx));
    }
    report("left", moves);
    if let Some(path) = shot {
        let image = cx.capture_screenshot(handle).expect("capture");
        image.save(path).expect("save the screenshot");
    }
}

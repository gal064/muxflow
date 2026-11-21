//! Typing latency and frame time of the Live view on a large file, to set the
//! size limit. Build it in release and run it where a Vulkan driver is
//! available (the QA sandbox's lavapipe is enough; nothing is rasterized
//! unless `--shot` asks for one):
//!
//!   typing_bench <file.md | prose:<bytes> | tables:<bytes>> [--at top|middle|end]
//!                [--fonts <dir>] [--keys <n>] [--shot <out.png>] [--find <word>]
//!
//! It opens the view in a headless window with the platform's real text
//! system, puts the caret at the top, middle or end of the document, and for
//! each key times the edit through the platform input handler and the frame
//! that follows (layout, prepaint and paint). Then it moves the caret, and
//! scrolls with the wheel. Prints p50, p95 and max of each. `prose:` and
//! `tables:` make a document of about that size: prose-heavy Markdown, or
//! Markdown with many tables.
//!
//! `--find` opens find on a word as the app's find bar does (count the matches
//! up to its limit, select the first, mark them), then steps through matches
//! one by one and by jumps to blocks never drawn, and types with the bar open
//! (each key also counts again, as the bar does).

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{
    AnyWindowHandle, AppContext, EntityInputHandler, Focusable, HeadlessAppContext, Hsla,
    PlatformInput, ScrollDelta, ScrollEvent, Window, assets::Assets, hsla, point, px, size,
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

/// A fixed pseudo-random sequence, so the same size gives the same document.
struct Words(u64);

impl Words {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize % n
    }

    fn sentence(&mut self) -> String {
        let count = 6 + self.next(14);
        let mut out = String::new();
        for i in 0..count {
            let word = WORDS[self.next(WORDS.len())];
            if i > 0 {
                out.push(' ');
            }
            match self.next(14) {
                0 => out.push_str(&format!("**{word}**")),
                1 => out.push_str(&format!("*{word}*")),
                2 => out.push_str(&format!("`{word}`")),
                3 => out.push_str(&format!("[{word}](./docs/{word}.md)")),
                4 => out.push_str(&format!("[{word}][{word}]")),
                _ => out.push_str(word),
            }
        }
        out.push('.');
        out
    }
}

const WORDS: [&str; 16] = [
    "the",
    "session",
    "host",
    "terminal",
    "window",
    "reconnects",
    "after",
    "a",
    "change",
    "to",
    "layout",
    "file",
    "keeps",
    "its",
    "state",
    "when",
];

/// Prose-heavy Markdown, or Markdown with many tables, of about `bytes`.
fn generate(kind: &str, bytes: usize) -> String {
    let mut words = Words(1);
    let mut out = String::new();
    let mut section = 0;
    while out.len() < bytes {
        section += 1;
        out.push_str(&format!("## Section {section}\n\n"));
        if kind == "tables" {
            out.push_str(&words.sentence());
            out.push_str("\n\n| Name | Kind | State | Owner | Notes |\n|---|:---:|---|---:|---|\n");
            for row in 0..12 {
                let notes = words.sentence();
                out.push_str(&format!(
                    "| item {row} | `kind` | **ok** | {} | {notes} |\n",
                    words.next(1000)
                ));
            }
            out.push('\n');
            continue;
        }
        for _ in 0..3 {
            for _ in 0..3 + words.next(4) {
                out.push_str(&words.sentence());
                out.push(' ');
            }
            out.push_str("\n\n");
        }
        match words.next(4) {
            0 => {
                for _ in 0..4 {
                    out.push_str(&format!("- {}\n", words.sentence()));
                }
                out.push('\n');
            }
            1 => out.push_str("```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n"),
            2 => out.push_str(&format!("> {}\n\n", words.sentence())),
            _ => {}
        }
    }
    if kind != "tables" {
        for word in WORDS {
            out.push_str(&format!("[{word}]: ./docs/{word}.md\n"));
        }
    }
    out
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
    let mut at_place = "middle".to_owned();
    let mut find = None;
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
            "--at" => at_place = args.next().expect("--at top|middle|end"),
            "--find" => find = args.next(),
            other => file = Some(other.to_owned()),
        }
    }
    let file =
        file.expect("usage: typing_bench <file.md | prose:N | tables:N> [--at top|middle|end]");
    let text = match file.split_once(':') {
        Some((kind @ ("prose" | "tables"), bytes)) => {
            generate(kind, bytes.parse().expect("a size in bytes"))
        }
        _ => std::fs::read_to_string(&file).expect("read the file"),
    };
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
    // At the end of a line of text: near the top, in the middle, or the last.
    let fraction = match at_place.as_str() {
        "top" => 0.02,
        "end" => 1.,
        _ => 0.5,
    };
    let near = ((text.len() as f64 * fraction) as usize).min(text.len().saturating_sub(1));
    let line_end = text[..near].rfind('\n').unwrap_or(0);
    let at = text[..line_end].encode_utf16().count();
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.replace_text_in_range(Some(at..at), "", window, cx)
        })
    })
    .expect("place the caret");
    // The caret is scrolled into view on the frame after it moves.
    draw(&mut cx);
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
    report("key", totals);
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
    let mut scrolls = Vec::new();
    for step in 0..keys * 2 {
        // Down, then back up.
        let down = step < keys;
        let start = Instant::now();
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(px(800.), px(500.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(if down { -120. } else { 120. }))),
                    ..ScrollEvent::default()
                }),
                cx,
            );
        })
        .expect("scroll");
        scrolls.push(start.elapsed().as_secs_f64() * 1000. + draw(&mut cx));
    }
    report("scroll", scrolls);
    if let Some(word) = find {
        // The find bar's matcher for a plain query, case not matched.
        let matcher = regex::RegexBuilder::new(&regex::escape(&word))
            .case_insensitive(true)
            .multi_line(true)
            .build()
            .expect("a pattern");
        // What the bar does on a step or a key: takes the text, counts up to
        // its limit, and finds the next match from the selection or `from`.
        let search = |cx: &mut HeadlessAppContext, from: Option<usize>| {
            cx.update(|cx| {
                let view = view.read(cx);
                let text = view.text().to_owned();
                let count = matcher.find_iter(&text).take(1001).count();
                let from = from.unwrap_or(view.selection().range().end);
                let found = (matcher.find_at(&text, from))
                    .or_else(|| matcher.find(&text))
                    .map(|found| found.range());
                (count, found)
            })
        };
        let select = |cx: &mut HeadlessAppContext, found: Option<std::ops::Range<usize>>| {
            cx.update_window(handle, |_, _, cx| {
                view.update(cx, |view, cx| {
                    view.set_find(Some(matcher.clone()), cx);
                    if let Some(found) = found {
                        view.select_match(found, cx);
                    }
                })
            })
            .expect("select");
            // The match is scrolled into view on the frame after.
            draw(cx) + draw(cx)
        };
        let start = Instant::now();
        let (count, found) = search(&mut cx, Some(0));
        let open = start.elapsed().as_secs_f64() * 1000. + select(&mut cx, found);
        let count = if count > 1000 {
            "1000+".to_owned()
        } else {
            count.to_string()
        };
        println!("find {word:?}: {count} matches, open {open:.1} ms");
        let mut steps = Vec::new();
        for _ in 0..keys {
            let start = Instant::now();
            let (_, found) = search(&mut cx, None);
            steps.push(start.elapsed().as_secs_f64() * 1000. + select(&mut cx, found));
        }
        report("step", steps);
        // Down the file in jumps, to blocks never drawn.
        let len = cx.update(|cx| view.read(cx).text().len());
        let mut jumps = Vec::new();
        for k in 0..keys {
            let start = Instant::now();
            let (_, found) = search(&mut cx, Some(len * k / keys));
            jumps.push(start.elapsed().as_secs_f64() * 1000. + select(&mut cx, found));
        }
        report("jump", jumps);
        // Typing at the match the bar is on, its matches marked.
        let end = cx.update(|cx| view.read(cx).selection().range().end);
        select(&mut cx, Some(end..end));
        let mut typed = Vec::new();
        for _ in 0..keys {
            let start = Instant::now();
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.replace_text_in_range(None, "x", window, cx)
                })
            })
            .expect("type");
            search(&mut cx, None);
            typed.push(start.elapsed().as_secs_f64() * 1000. + draw(&mut cx));
        }
        report("key", typed);
    }
    if let Some(path) = shot {
        let image = cx.capture_screenshot(handle).expect("capture");
        image.save(path).expect("save the screenshot");
    }
}

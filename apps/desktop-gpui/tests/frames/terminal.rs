//! Paint IME composition through the terminal element and its real VT grid.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::Processor;
use gpui_kit::{AppContext, Focusable, HeadlessAppContext, size, test::TestWindowExt};
use muxflow_gpui::terminal::{TerminalLink, TerminalPane};
use terminal_engine::GridSize;

#[path = "../support/terminal_input.rs"]
mod fixture;

use fixture::{Fake, frame, metrics, screen_frame};

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let state = super::write_state(dir.path(), "dark", 1.0);
    let mut cx = super::context(&state);
    let fake = Fake::default();
    let link = cx.update(|cx| {
        cx.new(|cx| TerminalLink::new("ime-frame".into(), Arc::new(fake.clone()), cx))
    });
    let metrics = metrics();
    let pane_size = size(
        metrics.cell_width * 40. + metrics.scrollbar_width,
        metrics.cell_height * 6.,
    );
    let handle = cx
        .open_window(pane_size, |window, cx| {
            let probes = link.read(cx).probes();
            cx.new(|cx| TerminalPane::new("%1".into(), link.clone(), metrics, probes, window, cx))
        })
        .unwrap();
    let pane = cx.update(|cx| handle.entity(cx).unwrap());
    cx.update_window(handle.into(), |_, window, cx| {
        window.activate_window();
        window.focus(&pane.read(cx).focus_handle(cx), cx);
        link.update(cx, |link, cx| {
            link.set_client(Some("client-1".into()), 1);
            link.mount(
                "%1",
                &pane,
                GridSize {
                    columns: 40,
                    rows: 6,
                },
                cx,
            );
            link.forward_frame(frame(10, "terminal", 0, &7_u64.to_be_bytes()));
            link.forward_frame(screen_frame(1, "%1", 1, b"\x1b[H$ \x1b[1 q"));
        });
    })
    .unwrap();

    let foreground = super::rgb(muxflow_gpui::theme::dark().term_fg);
    let cursor = super::rgb(muxflow_gpui::theme::dark().term_cursor);
    let shoot = |cx: &mut HeadlessAppContext| {
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.capture_screenshot(handle.into())
            .expect("offscreen terminal frame")
    };
    let set_preedit = |cx: &mut HeadlessAppContext, text: Option<&'static str>| {
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.input.preedit = text.map(Into::into);
                cx.notify();
            })
        });
    };
    let feed = |cx: &mut HeadlessAppContext, bytes: &[u8]| {
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                let mut parser: Processor = Processor::new();
                parser.advance(&mut *pane.terminal().unwrap().lock(), bytes);
                pane.repaint_all(cx);
            })
        });
    };

    // The headless renderer uses 2x. The prompt leaves the cursor in column 2.
    let deadline = Instant::now() + Duration::from_secs(20);
    let cursor_on = loop {
        cx.advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        let shot = shoot(&mut cx);
        if shot.get_pixel(36, 2).0[..3] == cursor {
            break shot;
        }
        assert!(
            Instant::now() < deadline,
            "the seeded prompt did not draw its cursor"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(cursor_on.dimensions(), (668, 204));
    // The blink timer defers to recent output on the real clock.
    std::thread::sleep(Duration::from_millis(610));
    cx.advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    let cursor_off = shoot(&mut cx);
    assert_ne!(cursor_off.get_pixel(36, 2).0[..3], cursor);

    set_preedit(&mut cx, Some("测试"));
    let preedit_blink_off = shoot(&mut cx);
    assert_eq!(preedit_blink_off.get_pixel(36, 32).0[..3], foreground);
    cx.advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    let preedit_blink_on = shoot(&mut cx);
    assert_eq!(preedit_blink_on, preedit_blink_off);
    save!(preedit_blink_on, "ime-prompt");
    set_preedit(&mut cx, None);
    assert_eq!(shoot(&mut cx), cursor_on);

    // Hide the real cursor. The placeholder starts with a reverse-video cell.
    feed(&mut cx, b"\x1b[2J\x1b[H> \x1b[48;2;24;40;55;7mA\x1b[27;2mask Codex to do anything\x1b[0m\x1b[1;3H\x1b[?25l");
    let placeholder = shoot(&mut cx);
    save!(placeholder, "ime-hidden-before");
    assert_eq!(placeholder.get_pixel(36, 2).0[..3], foreground);

    set_preedit(&mut cx, Some("测试"));
    let hidden = shoot(&mut cx);
    let covered = 32..96;
    for x in covered.clone() {
        assert_eq!(
            hidden.get_pixel(x, 32).0[..3],
            foreground,
            "underline at {x}"
        );
    }
    assert!(hidden.enumerate_pixels().all(|(x, y, pixel)| {
        (covered.contains(&x) && y < 34) || pixel == placeholder.get_pixel(x, y)
    }));
    set_preedit(&mut cx, Some("    "));
    let blank = shoot(&mut cx);
    for x in covered {
        assert_eq!(
            blank.get_pixel(x, 16).0,
            [24, 40, 55, 255],
            "tinted fill at {x}"
        );
    }
    // Erasing the cells under the composition must produce the same frame.
    feed(&mut cx, b"\x1b[48;2;24;40;55m\x1b[4X\x1b[0m");
    set_preedit(&mut cx, Some("测试"));
    assert_eq!(shoot(&mut cx), hidden);
    save!(hidden, "ime-hidden");

    feed(&mut cx, b"\x1b[2J\x1b[2;25Hright edge > ");
    set_preedit(&mut cx, Some("discard测试!"));
    let overflow = shoot(&mut cx);
    save!(overflow, "ime-right-edge");
    set_preedit(&mut cx, Some("试!"));
    assert_eq!(shoot(&mut cx), overflow);
    for x in 592..640 {
        assert_eq!(
            overflow.get_pixel(x, 66).0[..3],
            foreground,
            "right edge at {x}"
        );
    }
    // The bundled ASCII glyph must appear in the last cell after the wide glyph.
    set_preedit(&mut cx, Some("试 "));
    let without_bang = shoot(&mut cx);
    assert!(
        (624..640).any(|x| {
            (34..66).any(|y| overflow.get_pixel(x, y) != without_bang.get_pixel(x, y))
        })
    );
    // JetBrains Mono includes this fullwidth brace, even without a CJK font.
    set_preedit(&mut cx, Some("｛!"));
    let brace = shoot(&mut cx);
    set_preedit(&mut cx, Some("  !"));
    let without_brace = shoot(&mut cx);
    assert!(
        (592..624)
            .any(|x| { (34..66).any(|y| brace.get_pixel(x, y) != without_brace.get_pixel(x, y)) })
    );

    // At the last column a wide glyph cannot cover the beam even if it is on.
    feed(&mut cx, b"\x1b[2;40H\x1b[6 q\x1b[?25h");
    set_preedit(&mut cx, None);
    let beam = shoot(&mut cx);
    assert_eq!(beam.get_pixel(625, 36).0[..3], cursor);
    set_preedit(&mut cx, Some("界"));
    let no_beam = shoot(&mut cx);
    feed(&mut cx, b"\x1b[?25l");
    assert_eq!(shoot(&mut cx), no_beam);
    assert_ne!(beam, no_beam);
    assert!(
        fake.take().is_empty(),
        "the preedit is local to the renderer"
    );
    println!("terminal IME frames: ok");
}

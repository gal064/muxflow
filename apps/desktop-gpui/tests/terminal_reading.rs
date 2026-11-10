//! Reading a terminal pane through GPUI's test platform: selecting and copying
//! with the pointer, the wheel walking into tmux's history, and find. The engine
//! runs on its own thread; the poll is timer-driven, as in `terminal.rs`.

use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use desktop_core::app_state::{PersistedAppState, ThemePreference};
use futures::FutureExt;
use futures::future::BoxFuture;
use gpui_kit::{
    AppContext, Entity, Focusable, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels,
    Point, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, font, point, px,
};
use muxflow_gpui::{
    app::Core,
    commands::{self, registry::Platform},
    terminal::{TerminalLink, TerminalPane, cell_metrics::CellMetrics, link::POLL_ACTIVE, reading},
    theme,
};
use serde_json::Value;
use terminal_engine::{EngineOutput, GridSize, HubLimits, TerminalReadingState};

#[derive(Default)]
struct Calls {
    seeds: usize,
    history: Vec<(u32, u32)>,
    input: Vec<Vec<u8>>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Calls>>);

impl muxflow_gpui::connection_ui::backend::TerminalBackend for Fake {
    fn spawn_engine(
        &self,
        outputs: Sender<EngineOutput>,
    ) -> Box<dyn muxflow_gpui::connection_ui::backend::EngineChannel> {
        Box::new(terminal_engine::spawn(HubLimits::default(), outputs))
    }

    fn request_seed(&self, _: String, _: String) -> BoxFuture<'static, Result<(), String>> {
        self.0.lock().unwrap().seeds += 1;
        async { Ok(()) }.boxed()
    }

    fn request_history(
        &self,
        _: String,
        _: String,
        lines: u32,
        skip: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.0.lock().unwrap().history.push((lines, skip));
        async { Ok(()) }.boxed()
    }

    fn set_pane_visibility(
        &self,
        _: String,
        _: String,
        _: bool,
        _: bool,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn resize_client(
        &self,
        _: String,
        _: u16,
        _: u16,
        _: String,
        _: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn send_input(&self, _: String, _: String, bytes: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().input.push(bytes);
        Ok(())
    }

    fn send_paste(&self, _: String, _: String, bytes: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().input.push(bytes);
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}

    fn set_terminal_colors(&self, _: String, _: String) -> Result<(), String> {
        Ok(())
    }

    fn tmux_action(&self, _: String, _: Value) -> BoxFuture<'static, Result<Value, String>> {
        async { Ok(Value::Null) }.boxed()
    }

    fn start_upload(
        &self,
        _: muxflow_gpui::terminal::integration::transfers::UploadRequest,
        _: Arc<dyn desktop_core::platform::EventSink>,
    ) -> Result<String, String> {
        Err("no uploads here".into())
    }

    fn cancel_upload(&self, _: String) -> Result<Value, String> {
        Ok(Value::Null)
    }
}

fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn seed_frame(sequence: u64, text: &[u8]) -> Vec<u8> {
    let mut payload = 1_u64.to_be_bytes().to_vec();
    payload.extend_from_slice(text);
    frame(1, "%1", sequence, &payload)
}

/// Output after the seed: each frame carries the generation it makes.
fn output_frame(sequence: u64, text: &[u8]) -> Vec<u8> {
    let mut payload = sequence.to_be_bytes().to_vec();
    payload.extend_from_slice(text);
    frame(2, "%1", sequence, &payload)
}

/// tmux's answer to a history request: rows above the screen, and how many
/// tmux holds in all.
fn history_frame(sequence: u64, history_size: u32, text: &[u8]) -> Vec<u8> {
    let mut payload = vec![1];
    payload.extend_from_slice(&history_size.to_be_bytes());
    payload.extend_from_slice(text);
    frame(18, "%1", sequence, &payload)
}

fn wait_until(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.executor().advance_clock(POLL_ACTIVE);
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn metrics() -> Rc<CellMetrics> {
    Rc::new(CellMetrics {
        font: font("JetBrains Mono"),
        font_size: px(13.),
        cell_width: px(8.),
        cell_height: px(17.),
        line_width: px(1.),
        scrollbar_width: px(14.),
    })
}

struct Mounted {
    fake: Fake,
    link: Entity<TerminalLink>,
    pane: Entity<TerminalPane>,
}

/// A pane on a 40x6 grid, seeded with `screen`, painted once.
fn mounted<'a>(cx: &'a mut TestAppContext, screen: &[u8]) -> (Mounted, &'a mut VisualTestContext) {
    mounted_with(cx, screen, None)
}

/// The same, with saved settings. Without them the state file is never
/// written, and settings read their defaults.
fn mounted_with<'a>(
    cx: &'a mut TestAppContext,
    screen: &[u8],
    saved: Option<&PersistedAppState>,
) -> (Mounted, &'a mut VisualTestContext) {
    let state = std::env::temp_dir().join(format!(
        "muxflow-reading-test-{}-{:?}{}.json",
        std::process::id(),
        std::thread::current().id(),
        if saved.is_some() { "-saved" } else { "" }
    ));
    if let Some(saved) = saved {
        std::fs::write(&state, serde_json::to_vec(saved).unwrap()).unwrap();
    }
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::apply(
            ThemePreference::Dark,
            1.0,
            gpui_kit::WindowAppearance::Dark,
            cx,
        );
        cx.set_global(Core::at(&state));
        // The store has read it.
        if saved.is_some() {
            std::fs::remove_file(&state).unwrap();
        }
        commands::init(cx);
        // Copy on select is read from the live Settings.
        muxflow_gpui::settings::init(cx);
        reading::init(cx);
    });
    let fake = Fake::default();
    let backend: Arc<dyn muxflow_gpui::connection_ui::backend::TerminalBackend> =
        Arc::new(fake.clone());
    let link = cx.new(|cx| TerminalLink::new("qa".into(), backend, cx));
    let link_for_pane = link.clone();
    let (pane, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_pane.read(cx).probes();
        TerminalPane::new("%1".into(), link_for_pane, metrics(), probes, window, cx)
    });
    let size = GridSize {
        columns: 40,
        rows: 6,
    };
    link.update(visual, |link, _| {
        link.set_client(Some("client-1".into()), 1);
        link.mount("%1", &pane, size);
    });
    link.read_with(visual, |link, _| {
        link.forward_frame(frame(10, "terminal", 0, &7_u64.to_be_bytes()))
    });
    wait_until(visual, |_| fake.0.lock().unwrap().seeds > 0);
    link.read_with(visual, |link, _| link.forward_frame(seed_frame(1, screen)));
    // The first paint can come before the seed, whose reset would drop a
    // selection made in between. Every screen here ends in text, so the
    // cursor leaving the first column says the seed is on the grid.
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| {
            pane.painted_at().is_some()
                && pane.terminal().unwrap().lock().grid().cursor.point.column.0 > 0
        })
    });
    (Mounted { fake, link, pane }, visual)
}

/// The middle of a cell's left or right half, in window coordinates.
fn at(
    pane: &Entity<TerminalPane>,
    cx: &VisualTestContext,
    row: usize,
    column: usize,
    right: bool,
) -> Point<Pixels> {
    let painted = pane.read_with(cx, |pane, _| pane.painted_at().unwrap());
    let x = column as f32 * 8. + if right { 6. } else { 2. };
    point(
        painted.origin.x + px(x),
        painted.origin.y + px(row as f32 * 17. + 8.),
    )
}

fn clipboard(cx: &VisualTestContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

#[gpui_kit::test]
fn a_drag_selects_and_copies_and_a_double_click_takes_the_word(cx: &mut TestAppContext) {
    let (Mounted { fake, pane, .. }, visual) =
        mounted(cx, b"\x1b[2J\x1b[Hhello world\r\nsecond line");
    let none = Modifiers::default();
    let from = at(&pane, visual, 0, 0, false);
    let to = at(&pane, visual, 0, 4, true);
    visual.simulate_mouse_down(from, MouseButton::Left, none);
    visual.simulate_mouse_move(to, MouseButton::Left, none);
    visual.simulate_mouse_up(to, MouseButton::Left, none);
    visual.run_until_parked();
    assert_eq!(
        clipboard(visual).as_deref(),
        Some("hello"),
        "copy on select"
    );

    let word = at(&pane, visual, 1, 9, false);
    visual.simulate_event(MouseDownEvent {
        position: word,
        modifiers: none,
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    visual.simulate_event(MouseUpEvent {
        position: word,
        modifiers: none,
        button: MouseButton::Left,
        click_count: 2,
    });
    visual.run_until_parked();
    assert_eq!(clipboard(visual).as_deref(), Some("line"));

    // Typing drops the selection, as xterm does: on Linux that makes the next
    // Ctrl+C the interrupt.
    visual.simulate_input("x");
    visual.run_until_parked();
    assert_eq!(fake.0.lock().unwrap().input, [b"x".to_vec()]);
    assert!(!selected(&pane, visual));
}

/// Focus and mouse reports go to the program, but they are not the user's
/// input: the selection and the view stay as they were.
#[gpui_kit::test]
fn focus_and_mouse_reports_leave_the_selection_and_the_view_alone(cx: &mut TestAppContext) {
    // `?1004h` asks for focus reports, `?1000h` for clicks.
    let (Mounted { fake, pane, .. }, visual) = mounted(cx, b"\x1b[?1004h\x1b[?1000hecho hi");
    let sent = |fake: &Fake| fake.0.lock().unwrap().input.clone();
    select_first_word(&pane, visual);
    // Focus and window activation reach `input::report_focus` through the pane.
    visual.update(|window, _| window.activate_window());
    let focus = pane.read_with(visual, |pane, cx| pane.focus_handle(cx));
    visual.update(|window, cx| window.focus(&focus, cx));
    visual.run_until_parked();
    assert_eq!(sent(&fake), [b"\x1b[I".to_vec()]);
    assert!(selected(&pane, visual));
    visual.deactivate_window();
    visual.run_until_parked();
    assert_eq!(sent(&fake).last(), Some(&b"\x1b[O".to_vec()));
    assert!(selected(&pane, visual));

    let click = at(&pane, visual, 0, 6, false);
    visual.simulate_mouse_down(click, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(click, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(sent(&fake).len(), 4, "a press and a release reported");
    assert!(selected(&pane, visual));
    assert_eq!(
        pane.read_with(visual, |pane, _| pane.reading_state()),
        TerminalReadingState::Live
    );
}

/// The underline under the pointer goes once output rewrites its line.
#[gpui_kit::test]
fn a_hovered_link_is_dropped_when_output_rewrites_its_line(cx: &mut TestAppContext) {
    let (Mounted { link, pane, .. }, visual) = mounted(cx, b"see https://example.com/x");
    let underlined = |visual: &VisualTestContext| {
        pane.read_with(visual, |pane, _| {
            let term = pane.terminal().unwrap().lock();
            reading::highlights(pane, &term)
                .iter()
                .any(|highlight| highlight.underline)
        })
    };
    visual.simulate_mouse_move(at(&pane, visual, 0, 10, false), None, Modifiers::default());
    visual.run_until_parked();
    assert!(underlined(visual));
    link.read_with(visual, |link, _| {
        link.forward_frame(output_frame(2, b"\r\x1b[Kplain text"))
    });
    wait_until(visual, |visual| !underlined(visual));
}

fn select_first_word(pane: &Entity<TerminalPane>, cx: &mut VisualTestContext) {
    pane.update(cx, |pane, _| {
        let start = alacritty_terminal::index::Point::new(Line(0), Column(0));
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(
            alacritty_terminal::index::Point::new(Line(0), Column(3)),
            Side::Right,
        );
        pane.terminal().unwrap().lock().selection = Some(selection);
    });
}

fn selected(pane: &Entity<TerminalPane>, cx: &VisualTestContext) -> bool {
    pane.read_with(cx, |pane, _| {
        pane.terminal().unwrap().lock().selection.is_some()
    })
}

fn copy(pane: &Entity<TerminalPane>, platform: Platform, cx: &mut VisualTestContext) {
    let pane = pane.clone();
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| reading::copy(pane, platform, window, cx));
    });
    cx.run_until_parked();
}

/// Ports of the clipboard chord cases in `TerminalPane.test.tsx`.
#[gpui_kit::test]
fn copy_takes_the_selection_first_and_mac_hands_the_chord_to_a_mouse_app(cx: &mut TestAppContext) {
    // `?1000h`: the program reads mouse clicks.
    let (Mounted { fake, pane, .. }, visual) = mounted(cx, b"\x1b[?1000hecho hi");
    let sent = |fake: &Fake| fake.0.lock().unwrap().input.clone();

    // Linux Ctrl+C copies a standing selection, then drops it so the next
    // press interrupts.
    select_first_word(&pane, visual);
    copy(&pane, Platform::Linux, visual);
    assert_eq!(clipboard(visual).as_deref(), Some("echo"));
    assert!(!selected(&pane, visual));
    // With nothing selected, Linux sends nothing even to a mouse app.
    copy(&pane, Platform::Linux, visual);
    assert!(sent(&fake).is_empty());

    // Mac copies the selection first even while the program reads the mouse,
    // and keeps it.
    visual.write_to_clipboard(gpui_kit::ClipboardItem::new_string("old".into()));
    select_first_word(&pane, visual);
    copy(&pane, Platform::Mac, visual);
    assert_eq!(clipboard(visual).as_deref(), Some("echo"));
    assert!(sent(&fake).is_empty());
    assert!(selected(&pane, visual));

    // With no selection of ours, the program's own selection gets Super+C.
    pane.update(visual, |pane, _| {
        pane.terminal().unwrap().lock().selection = None
    });
    copy(&pane, Platform::Mac, visual);
    assert_eq!(sent(&fake), [b"\x1b[99;9u".to_vec()]);
}

#[gpui_kit::test]
fn mac_copy_with_nothing_selected_does_nothing_when_the_program_ignores_the_mouse(
    cx: &mut TestAppContext,
) {
    let (Mounted { fake, pane, .. }, visual) = mounted(cx, b"echo hi");
    visual.write_to_clipboard(gpui_kit::ClipboardItem::new_string("old".into()));
    copy(&pane, Platform::Mac, visual);
    assert!(fake.0.lock().unwrap().input.is_empty());
    assert_eq!(clipboard(visual).as_deref(), Some("old"));
}

#[gpui_kit::test]
fn the_wheel_reads_history_and_asks_for_the_page_above_the_top(cx: &mut TestAppContext) {
    let (Mounted { fake, link, pane }, visual) = mounted(cx, b"\x1b[2J\x1b[Hscreen");
    // A seed prefetches the first page; tmux answers with more still above.
    wait_until(visual, |_| fake.0.lock().unwrap().history.len() == 1);
    let older: Vec<u8> = (0..12)
        .flat_map(|row| format!("older {row}\r\n").into_bytes())
        .collect();
    link.read_with(visual, |link, _| {
        link.forward_frame(history_frame(2, 9_000, &older))
    });
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| {
            pane.terminal().unwrap().lock().grid().history_size() >= 12
        })
    });

    let inside = at(&pane, visual, 2, 2, false);
    let wheel = |lines: f32, visual: &mut VisualTestContext| {
        visual.simulate_event(ScrollEvent {
            position: inside,
            delta: ScrollDelta::Lines(point(0., lines)),
            ..Default::default()
        });
        visual.run_until_parked();
    };
    wheel(2., visual);
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| {
            pane.reading_state() == TerminalReadingState::Reading
        })
    });
    assert_eq!(
        fake.0.lock().unwrap().history.len(),
        1,
        "not at the top yet"
    );

    wheel(100., visual);
    let (offset, history) = pane.read_with(visual, |pane, _| {
        let term = pane.terminal().unwrap().lock();
        (term.grid().display_offset(), term.grid().history_size())
    });
    assert_eq!(offset, history, "stopped at the top");
    wait_until(visual, |_| fake.0.lock().unwrap().history.len() == 2);

    wheel(-200., visual);
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| {
            pane.reading_state() == TerminalReadingState::Live
        })
    });
}

#[gpui_kit::test]
fn find_walks_matches_in_the_history_and_highlights_them(cx: &mut TestAppContext) {
    let mut screen = b"\x1b[2J\x1b[H".to_vec();
    screen.extend((0..20).flat_map(|row| {
        let word = if row == 2 { "Needle" } else { "hay" };
        format!("{word} {row}\r\n").into_bytes()
    }));
    screen.extend_from_slice(b"needle last");
    let (Mounted { pane, .. }, visual) = mounted(cx, &screen);
    visual.update(|window, cx| {
        window.focus(&pane.focus_handle(cx), cx);
        commands::run("terminal.search", window, cx);
    });
    visual.run_until_parked();
    visual.simulate_input("needle");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();

    // "Next" starts from the oldest row: the match up in the history.
    let (offset, rows) = pane.read_with(visual, |pane, _| {
        let term = pane.terminal().unwrap().lock();
        let found: Vec<usize> = reading::highlights(pane, &term)
            .iter()
            .filter(|highlight| highlight.bg.is_some())
            .map(|highlight| highlight.row)
            .collect();
        (term.grid().display_offset(), found)
    });
    assert!(offset > 0, "scrolled to the match");
    assert_eq!(rows.len(), 1, "one match on screen: {rows:?}");

    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    let offset = pane.read_with(visual, |pane, _| {
        pane.terminal().unwrap().lock().grid().display_offset()
    });
    assert_eq!(offset, 0, "the next match is on the live screen");

    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    let (focused, highlighted) = visual.update(|window, cx| {
        let pane = pane.read(cx);
        let term = pane.terminal().unwrap().lock();
        (
            pane.focus_handle(cx).is_focused(window),
            reading::highlights(pane, &term).len(),
        )
    });
    assert!(focused, "focus returns to the terminal");
    assert_eq!(highlighted, 0, "closing find clears its highlights");
}

#[gpui_kit::test]
fn a_selection_output_drops_is_unpainted_from_rows_the_output_did_not_touch(
    cx: &mut TestAppContext,
) {
    let (Mounted { link, pane, .. }, visual) = mounted(cx, b"one\r\ntwo\r\nthree");
    let row = |visual: &VisualTestContext| pane.read_with(visual, |pane, _| pane.row_spec(0));
    let plain = row(visual);
    pane.update(visual, |pane, cx| {
        let mut selection = Selection::new(
            SelectionType::Simple,
            alacritty_terminal::index::Point::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(
            alacritty_terminal::index::Point::new(Line(2), Column(2)),
            Side::Right,
        );
        pane.terminal().unwrap().lock().selection = Some(selection);
        pane.repaint_all(cx);
    });
    visual.run_until_parked();
    assert_ne!(row(visual), plain, "the selection is painted");
    // Erasing the cursor's line drops the selection and damages only that line.
    link.read_with(visual, |link, _| {
        link.forward_frame(output_frame(2, b"\r\x1b[K"))
    });
    wait_until(visual, |cx| !selected(&pane, cx));
    wait_until(visual, |visual| row(visual) == plain);
}

#[gpui_kit::test]
fn the_link_modifier_opens_links_in_a_mouse_program_and_otherwise_selects_nothing(
    cx: &mut TestAppContext,
) {
    // `?1000h`: the program reads mouse clicks.
    let (Mounted { fake, pane, .. }, visual) =
        mounted(cx, b"\x1b[?1000hplain https://example.com/x");
    let found = |column: usize, visual: &mut VisualTestContext| {
        let position = at(&pane, visual, 0, column, false);
        pane.read_with(visual, |pane, _| reading::link_at(pane, position))
    };
    assert!(found(10, visual), "a link under the pointer");
    assert!(!found(1, visual));

    // Off a link, the press is the pane grab's: no selection, no report.
    let link_modifier = if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    };
    let off = at(&pane, visual, 0, 1, false);
    let further = at(&pane, visual, 0, 4, true);
    visual.simulate_mouse_down(off, MouseButton::Left, link_modifier);
    visual.simulate_mouse_move(further, MouseButton::Left, link_modifier);
    visual.simulate_mouse_up(further, MouseButton::Left, link_modifier);
    visual.run_until_parked();
    assert!(!selected(&pane, visual));
    assert!(fake.0.lock().unwrap().input.is_empty());
}

#[gpui_kit::test]
fn with_copy_on_select_off_a_selection_waits_for_the_copy_command(cx: &mut TestAppContext) {
    let mut saved = PersistedAppState::default();
    saved.shell.copy_on_select = Some(false);
    let (Mounted { pane, .. }, visual) = mounted_with(cx, b"hello world", Some(&saved));
    visual.write_to_clipboard(gpui_kit::ClipboardItem::new_string("old".into()));
    let none = Modifiers::default();
    let (from, to) = (
        at(&pane, visual, 0, 0, false),
        at(&pane, visual, 0, 4, true),
    );
    visual.simulate_mouse_down(from, MouseButton::Left, none);
    visual.simulate_mouse_move(to, MouseButton::Left, none);
    visual.simulate_mouse_up(to, MouseButton::Left, none);
    visual.run_until_parked();
    assert_eq!(clipboard(visual).as_deref(), Some("old"));
    copy(&pane, Platform::Mac, visual);
    assert_eq!(clipboard(visual).as_deref(), Some("hello"));
}

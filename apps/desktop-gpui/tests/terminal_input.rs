//! Keyboard, mouse and paste input through GPUI's test platform, against a
//! real engine thread that parses the program's mode changes. Polled on virtual
//! time, as `terminal.rs` is, so the engine thread never wakes a GPUI task.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use desktop_core::app_state::ThemePreference;
use futures::FutureExt;
use futures::future::BoxFuture;
use gpui_kit::{
    AppContext, Context, Entity, EntityInputHandler, Focusable, IntoElement, Modifiers,
    MouseButton, ParentElement, Pixels, Point, Render, ScrollDelta, ScrollEvent, Styled,
    TestAppContext, VisualTestContext, Window, base::Root, div, font, point, px,
};
use muxflow_gpui::{
    connection_ui::backend::{EngineChannel, TerminalBackend},
    terminal::{self, TerminalLink, TerminalPane, cell_metrics::CellMetrics, link::POLL_ACTIVE},
    theme,
};
use serde_json::Value;
use terminal_engine::{EngineOutput, GridSize, HubLimits};

/// One send, as `(pane, bytes)`.
type Sent = (String, Vec<u8>);

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Vec<Sent>>>);

impl Fake {
    fn take(&self) -> Vec<Sent> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

impl TerminalBackend for Fake {
    fn spawn_engine(&self, outputs: Sender<EngineOutput>) -> Box<dyn EngineChannel> {
        Box::new(terminal_engine::spawn(HubLimits::default(), outputs))
    }

    fn request_seed(&self, _: String, _: String) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn request_history(
        &self,
        _: String,
        _: String,
        _: u32,
        _: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
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

    fn send_input(&self, _: String, pane_id: String, bytes: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().push((pane_id, bytes));
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}
}

fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// A seed (kind 1) or an output (kind 2) for `pane`.
fn screen_frame(kind: u8, pane: &str, generation: u64, text: &[u8]) -> Vec<u8> {
    let mut payload = generation.to_be_bytes().to_vec();
    payload.extend_from_slice(text);
    frame(kind, pane, generation, &payload)
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

struct Split {
    panes: Vec<Entity<TerminalPane>>,
}

impl Render for Split {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().children(
            self.panes
                .iter()
                .map(|pane| div().w_1_2().h_full().child(pane.clone())),
        )
    }
}

/// Panes `%1`, `%2`, … side by side, mounted, seeded and painted.
fn panes(
    count: usize,
    cx: &mut TestAppContext,
) -> (
    Fake,
    Entity<TerminalLink>,
    Vec<Entity<TerminalPane>>,
    &mut VisualTestContext,
) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::apply(
            ThemePreference::Dark,
            1.0,
            gpui_kit::WindowAppearance::Dark,
            cx,
        );
        terminal::init(cx);
    });
    let fake = Fake::default();
    let backend: Arc<dyn TerminalBackend> = Arc::new(fake.clone());
    let link = cx.new(|cx| TerminalLink::new("qa".into(), backend, cx));
    let link_for_panes = link.clone();
    // Under Kit's root, as in the app, so its own Tab and Ctrl+C bindings are
    // in the dispatch path.
    let made = Rc::new(RefCell::new(Vec::new()));
    let made_in_window = made.clone();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let probes = link_for_panes.read(cx).probes();
        let panes: Vec<_> = (1..=count)
            .map(|index| {
                let (link, probes) = (link_for_panes.clone(), probes.clone());
                cx.new(|cx| {
                    TerminalPane::new(
                        format!("%{index}").into(),
                        link,
                        metrics(),
                        probes,
                        window,
                        cx,
                    )
                })
            })
            .collect();
        made_in_window.borrow_mut().clone_from(&panes);
        let split = cx.new(|_| Split { panes });
        Root::new(split, window, cx)
    });
    let panes = made.borrow().clone();
    // Focus events carry no path in a window that is not active.
    visual.update(|window, _| window.activate_window());
    let size = GridSize {
        columns: 40,
        rows: 6,
    };
    link.update(visual, |link, _| {
        link.set_client(Some("client-1".into()), 1);
        for (index, pane) in panes.iter().enumerate() {
            link.mount(&format!("%{}", index + 1), pane, size);
        }
        link.forward_frame(frame(10, "terminal", 0, &7_u64.to_be_bytes()));
    });
    wait_until(visual, |cx| {
        panes
            .iter()
            .all(|pane| pane.read_with(cx, |pane, _| pane.terminal().is_some()))
    });
    for index in 1..=count {
        output(&link, visual, &format!("%{index}"), 1, b"\x1b[2J\x1b[H$ ");
    }
    wait_until(visual, |cx| {
        panes
            .iter()
            .all(|pane| pane.read_with(cx, |pane, _| pane.painted_at().is_some()))
    });
    fake.take();
    (fake, link, panes, visual)
}

/// Program output for `pane`; generation 1 is the seed.
fn output(
    link: &Entity<TerminalLink>,
    cx: &mut VisualTestContext,
    pane: &str,
    generation: u64,
    text: &[u8],
) {
    let kind = if generation == 1 { 1 } else { 2 };
    link.read_with(cx, |link, _| {
        link.forward_frame(screen_frame(kind, pane, generation, text));
    });
}

fn focus(pane: &Entity<TerminalPane>, cx: &mut VisualTestContext) {
    let handle = pane.read_with(cx, |pane, cx| pane.focus_handle(cx));
    cx.update(|window, cx| window.focus(&handle, cx));
    // Focus and blur listeners run when the next frame is drawn.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// The middle of a cell, as last painted.
fn cell(
    pane: &Entity<TerminalPane>,
    cx: &mut VisualTestContext,
    column: f32,
    row: f32,
) -> Point<Pixels> {
    let painted = pane.read_with(cx, |pane, _| pane.painted_at().expect("painted"));
    point(
        painted.origin.x + painted.cell.width * (column + 0.5),
        painted.origin.y + painted.cell.height * (row + 0.5),
    )
}

fn sent(pane: &str, bytes: &[u8]) -> Sent {
    (pane.to_owned(), bytes.to_vec())
}

#[gpui_kit::test]
fn keys_reach_only_the_focused_pane(cx: &mut TestAppContext) {
    let (fake, _link, panes, visual) = panes(2, cx);
    focus(&panes[0], visual);
    visual.simulate_keystrokes("a ctrl-/ enter shift-enter ctrl-c tab up");
    visual.simulate_input("é");
    assert_eq!(
        fake.take(),
        [
            sent("%1", b"a"),
            sent("%1", b"\x1f"),
            sent("%1", b"\r"),
            sent("%1", b"\x1b[13;2u"),
            sent("%1", b"\x03"),
            sent("%1", b"\t"),
            sent("%1", b"\x1b[A"),
            sent("%1", "é".as_bytes()),
        ]
    );
    focus(&panes[1], visual);
    visual.simulate_keystrokes("b");
    assert_eq!(fake.take(), [sent("%2", b"b")]);
}

#[gpui_kit::test]
fn a_composition_holds_keys_until_it_commits_or_focus_leaves(cx: &mut TestAppContext) {
    let (fake, _link, panes, visual) = panes(2, cx);
    focus(&panes[0], visual);
    let compose = |text: &str, visual: &mut VisualTestContext| {
        panes[0].update_in(visual, |pane, window, cx| {
            pane.replace_and_mark_text_in_range(None, text, None, window, cx);
        });
    };
    compose("k", visual);
    // The composition's keys are the IME's, and only the commit is sent. An
    // arrow, since the test platform would type Enter's text into the field.
    visual.simulate_keystrokes("up");
    panes[0].update_in(visual, |pane, window, cx| {
        pane.replace_text_in_range(None, "か", window, cx);
    });
    assert_eq!(fake.take(), [sent("%1", "か".as_bytes())]);

    // A composition abandoned by a focus change does not hold the pane.
    compose("k", visual);
    focus(&panes[1], visual);
    focus(&panes[0], visual);
    visual.simulate_keystrokes("enter");
    assert_eq!(fake.take(), [sent("%1", b"\r")]);
}

#[gpui_kit::test]
fn mouse_reports_and_pastes_follow_the_modes_the_program_asked_for(cx: &mut TestAppContext) {
    let (fake, link, panes, visual) = panes(1, cx);
    let pane = &panes[0];
    // Not reading the mouse: a click is the reading side's, and sends nothing.
    let at = cell(pane, visual, 2., 1.);
    visual.simulate_click(at, Modifiers::none());
    assert_eq!(fake.take(), []);

    // Button-event tracking (1002) in the SGR encoding (1006), and bracketed
    // paste (2004).
    output(&link, visual, "%1", 2, b"\x1b[?1002h\x1b[?1006h\x1b[?2004h");
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| pane.mouse_reporting())
    });
    visual.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    // A drag reaches the program in each new cell, and past the pane's edge
    // as the nearest cell.
    let (next, same, hover) = (
        cell(pane, visual, 3., 1.),
        cell(pane, visual, 3.2, 1.),
        cell(pane, visual, 5., 2.),
    );
    visual.simulate_mouse_move(next, MouseButton::Left, Modifiers::none());
    visual.simulate_mouse_move(same, MouseButton::Left, Modifiers::none());
    visual.simulate_mouse_move(
        point(px(-50.), px(-50.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    visual.simulate_mouse_up(
        point(px(-50.), px(-50.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    // Motion with no button held is not asked for in this mode.
    visual.simulate_mouse_move(hover, None, Modifiers::none());
    visual.simulate_event(ScrollEvent {
        position: at,
        delta: ScrollDelta::Lines(point(0., 1.)),
        ..Default::default()
    });
    assert_eq!(
        fake.take(),
        [
            sent("%1", b"\x1b[<0;3;2M"),
            sent("%1", b"\x1b[<32;4;2M"),
            sent("%1", b"\x1b[<32;1;1M"),
            sent("%1", b"\x1b[<0;1;1m"),
            sent("%1", b"\x1b[<64;3;2M"),
        ]
    );
    // Shift forces a local selection, so the program hears nothing, but a
    // Shift wheel is the program's, with its modifier.
    visual.simulate_click(at, Modifiers::shift());
    visual.simulate_event(ScrollEvent {
        position: at,
        delta: ScrollDelta::Lines(point(0., -1.)),
        modifiers: Modifiers::shift(),
        ..Default::default()
    });
    assert_eq!(fake.take(), [sent("%1", b"\x1b[<69;3;2M")]);

    // A release that never reached the pane (hidden mid-drag) is owed anyway:
    // the next move with no button held ends the press.
    visual.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    visual.simulate_mouse_move(hover, None, Modifiers::none());
    visual.simulate_mouse_move(at, None, Modifiers::none());
    assert_eq!(
        fake.take(),
        [sent("%1", b"\x1b[<0;3;2M"), sent("%1", b"\x1b[<0;6;3m")]
    );

    pane.update(visual, |pane, cx| {
        terminal::input::paste(pane, "one\ntwo\x1b[201~", cx)
    });
    assert_eq!(fake.take(), [sent("%1", b"\x1b[200~one\rtwo\x1b[201~")]);
    output(&link, visual, "%1", 3, b"\x1b[?2004l");
    wait_until(visual, |cx| {
        pane.update(cx, |pane, cx| terminal::input::paste(pane, "x\r\ny", cx));
        fake.take() == [sent("%1", b"x\ry")]
    });

    // Focus reports (1004) follow the window as well as the pane.
    output(&link, visual, "%1", 4, b"\x1b[?1004h");
    wait_until(visual, |cx| {
        pane.read_with(cx, |pane, _| {
            let mode = *pane.terminal().expect("mounted").lock().mode();
            mode.contains(TermMode::FOCUS_IN_OUT)
        })
    });
    visual.deactivate_window();
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    assert_eq!(fake.take(), [sent("%1", b"\x1b[O"), sent("%1", b"\x1b[I")]);
}

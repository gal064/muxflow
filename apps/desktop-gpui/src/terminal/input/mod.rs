//! T3.4: keyboard, IME, paste and mouse reporting, and the input latency
//! probes (`echoLagProbe`, `inputLatencyStats`), which need the input path.
//!
//! A key press goes, in order, to:
//!
//! 1. The app's shortcuts (`commands`), which see it first, as the window's
//!    capture listener did in the webview.
//! 2. GPUI key bindings. Kit's root binds Tab and Ctrl+C for itself; inside a
//!    pane ([`KEY_CONTEXT`]) those keys are the program's. The context covers
//!    fields drawn inside the pane too, which bind those keys themselves if
//!    they need them.
//! 3. [`policy`], the shortcuts Muxflow owns in a terminal, then [`keys`], the
//!    encoding of every key that has no text.
//! 4. The platform's text input ([`ime`]), for everything that types text.
//!
//! Every byte for the program, typed, pasted or reported, leaves through
//! `send`.

pub mod ime;
pub mod keys;
pub mod latency;
pub mod mouse;
pub mod policy;

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::TermMode;
use gpui_kit::{
    App, Bounds, Context, DispatchPhase, Div, ElementInputHandler, Entity, Focusable,
    InteractiveElement, KeyBinding, KeyDownEvent, Modifiers, MouseButton, MouseMoveEvent, NoAction,
    Pixels, Point, ScrollDelta, SharedString, Stateful, Task, Window,
};
use serde_json::json;

use crate::commands::{self, registry::Platform};
use crate::connection_ui::backend::TerminalBackend;

use super::link::LinkEvent;
use super::pane::TerminalPane;
use latency::{EchoLagProbe, EchoOutput, InputLatency, LinkCounters, Segment, record_perf};
use mouse::{MouseReport, ReportAction, ReportButton, encode_mouse};
use policy::{ClipboardAlias, KeyContext, terminal_clipboard_alias, translate_terminal_key};

pub use ime::is_composing;

/// The key context of a pane.
pub const KEY_CONTEXT: &str = "Terminal";

/// A send slower than this says the stall is on the way out of the app.
const SLOW_SEND: Duration = Duration::from_millis(250);
const SLOW_SEND_INTERVAL: Duration = Duration::from_secs(10);

/// At most one paint sample per pane per interval, so a pane repainting at full
/// speed cannot make its own measurement expensive.
const PAINT_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

pub fn init(cx: &mut App) {
    // Kit's root moves focus on Tab and copies UI text on Ctrl+C. In a
    // terminal both are the program's keys.
    cx.bind_keys([
        KeyBinding::new("tab", NoAction, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-c", NoAction, Some(KEY_CONTEXT)),
    ]);
}

/// This feature's state on a pane.
#[derive(Default)]
pub struct PaneInput {
    /// Text an IME is composing. The element draws it at the cursor.
    pub preedit: Option<SharedString>,
    echo: EchoLagProbe,
    echo_timeout: Option<Task<()>>,
    journal: Option<Journal>,
    last_slow_send: Option<Instant>,
    last_paint_sample: Cell<Option<Instant>>,
    mouse: MouseState,
    /// Whether the pane had focus at its last focus change. A pane that never
    /// had focus has nothing to report when the window loses it.
    focused: bool,
}

#[derive(Default)]
struct MouseState {
    /// The button whose press the program was sent, owed a release.
    pressed: Option<ReportButton>,
    /// The cell of the last report, so motion inside a cell sends nothing.
    cell: Option<(usize, usize)>,
    /// Wheel travel short of a whole line.
    wheel: f32,
}

/// Where a pane's measurements go: the client its last input went to.
struct Journal {
    pane_id: SharedString,
    backend: Arc<dyn TerminalBackend>,
    client_id: String,
    connection_epoch: u64,
    latency: InputLatency,
}

impl Journal {
    /// Only a measured process reads the link's counters per keystroke.
    fn counters(&self) -> Option<LinkCounters> {
        if !desktop_core::perf_log::sink::perf_log_enabled() {
            return None;
        }
        LinkCounters::from_stats(&self.backend.link_stats(&self.client_id)?)
    }

    /// An echo incident carries the link's state, which is what tells a full
    /// delivery window from a quiet link.
    fn record(&self, (kind, mut detail): (&'static str, serde_json::Value)) {
        if let Some(stats) = self.backend.link_stats(&self.client_id) {
            latency::extend(&mut detail, stats);
        }
        self.backend.record_incident(kind, detail);
    }
}

impl PaneInput {
    /// A `%output` reached this pane's screen, in stream order. Returns the lag
    /// of an echo the journal recorded as slow, for the link quality verdict.
    pub fn note_output(&mut self, sequence: u64, generation: u64) -> Option<u64> {
        let journal = self.journal.as_ref()?;
        let output = desktop_core::perf_log::sink::perf_log_enabled().then_some(EchoOutput {
            sequence,
            generation,
            connection_epoch: journal.connection_epoch,
        });
        let outcome = self
            .echo
            .note_output(&journal.pane_id, Instant::now(), output, || {
                journal.counters()
            });
        let lag = outcome.sample?;
        self.echo_timeout = None;
        journal.latency.sample(Segment::EndToEnd, lag);
        if let Some(echo) = outcome.echo {
            record_perf("perf.echo", echo);
        }
        let incident = outcome.incident?;
        let lag_ms = incident.1["lagMs"].as_u64();
        journal.record(incident);
        lag_ms
    }

    fn expire_echo(&mut self) {
        self.echo_timeout = None;
        let Some(journal) = self.journal.as_ref() else {
            return;
        };
        if let Some(incident) = self
            .echo
            .expire(&journal.pane_id, Instant::now(), || journal.counters())
        {
            journal.record(incident);
        }
    }
}

fn mode(pane: &TerminalPane) -> TermMode {
    pane.terminal()
        .map_or(TermMode::empty(), |terminal| *terminal.lock().mode())
}

/// The one path every byte for the program takes. A `paste` is delivered
/// through tmux's paste path; see [`paste`].
pub(super) fn send(
    pane: &mut TerminalPane,
    bytes: Vec<u8>,
    paste: bool,
    cx: &mut Context<TerminalPane>,
) -> Result<(), String> {
    let now = Instant::now();
    let (backend, client_id, connection_epoch) = pane.link().update(cx, |link, _| {
        link.note_input();
        (
            link.backend().clone(),
            link.client_id().map(str::to_owned),
            link.connection_epoch(),
        )
    });
    let client_id = client_id.ok_or("this terminal has no live client")?;
    let latency = InputLatency::global(cx);
    latency.note_client(&backend, &client_id, cx);
    let journal = Journal {
        pane_id: pane.pane_id().clone(),
        backend: backend.clone(),
        client_id: client_id.clone(),
        connection_epoch,
        latency: latency.clone(),
    };
    if pane.input.echo.note_input(now, || journal.counters()) {
        pane.input.echo_timeout = Some(cx.spawn(async move |pane, cx| {
            cx.background_executor().timer(latency::ECHO_TIMEOUT).await;
            pane.update(cx, |pane, _| pane.input.expire_echo()).ok();
        }));
    }
    pane.input.journal = Some(journal);

    let sent_at = Instant::now();
    let pane_id = pane.pane_id().to_string();
    if paste {
        backend.send_paste(client_id, pane_id, bytes)?;
    } else {
        backend.send_input(client_id, pane_id, bytes)?;
    }
    // How long the input took to leave the app: the `send` segment of the
    // keystroke's wait, and a slow one says the stall is on this side.
    let took = sent_at.elapsed();
    latency.sample(Segment::Send, took);
    if took > SLOW_SEND
        && pane
            .input
            .last_slow_send
            .is_none_or(|at| sent_at.saturating_duration_since(at) >= SLOW_SEND_INTERVAL)
    {
        pane.input.last_slow_send = Some(sent_at);
        backend.record_incident(
            "input.sendSlow",
            json!({ "paneId": pane.pane_id().as_ref(), "ms": took.as_millis() as u64 }),
        );
    }
    Ok(())
}

/// Input the user produced. With no live client there is nothing to send to
/// and nothing to say; a failed send is said in the status line.
fn send_user_input(
    pane: &mut TerminalPane,
    bytes: Vec<u8>,
    paste: bool,
    cx: &mut Context<TerminalPane>,
) {
    if pane.link().read(cx).client_id().is_none() {
        return;
    }
    if let Err(error) = send(pane, bytes, paste, cx) {
        pane.link()
            .update(cx, |_, cx| cx.emit(LinkEvent::Status(error)));
    }
}

/// Typing, by key or by IME commit: the input the echo probe measures.
fn send_typed(pane: &mut TerminalPane, bytes: Vec<u8>, cx: &mut Context<TerminalPane>) {
    pane.input.echo.note_key(Instant::now());
    super::reading::before_user_input(pane, cx);
    send_user_input(pane, bytes, false, cx);
}

/// Pastes `text` into the program, as mobile does: the host pastes it through
/// tmux, which brackets it iff the program asked (DEC 2004). Only tmux knows
/// that reliably; this pane's copy of the mode can be stale, as after a seed
/// from tmux 3.4 or older, which cannot report it.
///
/// tmux turns each LF into the CR a terminal types for Enter, so a CRLF line
/// end becomes one LF first, or it would arrive as two Enters. An end marker
/// inside the text is removed, so the text cannot end the paste early.
pub fn paste(pane: &mut TerminalPane, text: &str, cx: &mut Context<TerminalPane>) {
    const END: &str = "\x1b[201~";
    let mut text = text.to_owned();
    while text.contains(END) {
        text = text.replace(END, "");
    }
    super::reading::before_user_input(pane, cx);
    send_user_input(pane, text.replace("\r\n", "\n").into_bytes(), true, cx);
}

/// Tells a program that asked (DEC 1004) when the pane gains or loses focus,
/// the window's included. Called by the pane on both.
pub fn report_focus(pane: &mut TerminalPane, window: &Window, cx: &mut Context<TerminalPane>) {
    // A composition open when focus moved away is over: the IME carries on
    // with the newly focused input, and a stale preedit would hold this pane's
    // keys and the app's shortcuts for the IME.
    if !pane.focus_handle(cx).is_focused(window) {
        ime::set_preedit(pane, None, cx);
    }
    let focused = pane.focus_handle(cx).is_focused(window) && window.is_window_active();
    if std::mem::replace(&mut pane.input.focused, focused) == focused
        || !mode(pane).contains(TermMode::FOCUS_IN_OUT)
    {
        return;
    }
    let report = if focused { b"\x1b[I" } else { b"\x1b[O" };
    send_user_input(pane, report.to_vec(), false, cx);
}

fn on_key_down(
    pane: &Entity<TerminalPane>,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let keystroke = &event.keystroke;
    // A field drawn inside the pane, such as the find bar, keeps its keys: a
    // key it leaves unhandled bubbles here, and must not reach the program.
    if keystroke.is_modifier_key() || !pane.focus_handle(cx).is_focused(window) {
        return;
    }
    let (composing, mode) = {
        let pane = pane.read(cx);
        (pane.input.preedit.is_some(), mode(pane))
    };
    // A key inside a composition belongs to the IME.
    if composing {
        return;
    }
    let platform = Platform::current();
    // A key that becomes no bytes here still vouches for the text it types.
    pane.update(cx, |pane, _| pane.input.echo.note_key(Instant::now()));
    let (profile_id, pane_id) = {
        let pane = pane.read(cx);
        (
            pane.link().read(cx).profile_id().to_owned(),
            pane.pane_id().clone(),
        )
    };
    super::integration::agents::note_activity(&profile_id, &pane_id, cx);
    if let Some(alias) = terminal_clipboard_alias(keystroke, platform) {
        // Claimed outright, so the chord never also reaches the program.
        cx.stop_propagation();
        let id = match alias {
            ClipboardAlias::Copy => "terminal.copy",
            ClipboardAlias::Paste => "terminal.paste",
        };
        commands::run(id, window, cx);
        return;
    }
    let context = KeyContext {
        alternate_screen: mode.contains(TermMode::ALT_SCREEN),
        application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
        platform,
        composing,
    };
    let bytes = translate_terminal_key(keystroke, context)
        .map(<[u8]>::to_vec)
        .or_else(|| keys::encode_key(keystroke, context.application_cursor_keys, platform));
    let Some(bytes) = bytes else {
        return;
    };
    cx.stop_propagation();
    pane.update(cx, |pane, cx| {
        super::reading::before_user_input(pane, cx);
        send_user_input(pane, bytes, false, cx);
    });
}

/// Shift, and Option on macOS, select text even while the program reads the
/// mouse: xterm's force-selection rule. Those presses are the reading side's.
pub fn forces_local_selection(modifiers: &Modifiers) -> bool {
    modifiers.shift || (Platform::current() == Platform::Mac && modifiers.alt)
}

/// Whether a press is this module's: the program reads the mouse, and the
/// press is neither a forced selection nor the Ctrl (Cmd on macOS) left press
/// that grabs a pane or opens a link. Wheel and motion need only the first, as
/// in xterm, which forced selection on a press alone.
pub fn claims_press(pane: &TerminalPane, button: MouseButton, modifiers: &Modifiers) -> bool {
    let secondary = match Platform::current() {
        Platform::Mac => modifiers.platform,
        Platform::Linux => modifiers.control,
    };
    pane.mouse_reporting()
        && !forces_local_selection(modifiers)
        && !(secondary && button == MouseButton::Left)
}

fn report_button(button: MouseButton) -> Option<ReportButton> {
    match button {
        MouseButton::Left => Some(ReportButton::Left),
        MouseButton::Middle => Some(ReportButton::Middle),
        MouseButton::Right => Some(ReportButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

/// The cell under `position`, clamped to the grid.
fn mouse_cell(pane: &TerminalPane, position: Point<Pixels>) -> Option<(usize, usize)> {
    let painted = pane.painted_at()?;
    let term = pane.terminal()?.lock();
    let along = |offset: Pixels, cell: Pixels, cells: usize| {
        let index = (f32::from(offset) / f32::from(cell)).floor().max(0.) as usize;
        index.min(cells.saturating_sub(1))
    };
    Some((
        along(
            position.x - painted.origin.x,
            painted.cell.width,
            term.columns(),
        ),
        along(
            position.y - painted.origin.y,
            painted.cell.height,
            term.screen_lines(),
        ),
    ))
}

fn report_mouse(
    pane: &mut TerminalPane,
    button: ReportButton,
    action: ReportAction,
    position: Point<Pixels>,
    modifiers: Modifiers,
    cx: &mut Context<TerminalPane>,
) {
    let Some((column, row)) = mouse_cell(pane, position) else {
        return;
    };
    pane.input.mouse.cell = Some((column, row));
    let report = MouseReport {
        button,
        action,
        column,
        row,
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
    };
    if let Some(bytes) = encode_mouse(report, mode(pane)) {
        send_user_input(pane, bytes, false, cx);
    }
}

/// Motion into a new cell: a drag while a press is owed its release, else a
/// hover. The mode decides whether either is reported.
fn report_motion(
    pane: &mut TerminalPane,
    position: Point<Pixels>,
    modifiers: Modifiers,
    cx: &mut Context<TerminalPane>,
) {
    if mouse_cell(pane, position) == pane.input.mouse.cell {
        return;
    }
    let button = pane.input.mouse.pressed.unwrap_or(ReportButton::None);
    report_mouse(pane, button, ReportAction::Motion, position, modifiers, cx);
}

/// A press the program was sent is owed its release, wherever it happens.
fn release_mouse(
    pane: &mut TerminalPane,
    button: Option<ReportButton>,
    position: Point<Pixels>,
    modifiers: Modifiers,
    cx: &mut Context<TerminalPane>,
) {
    if button.is_none() || pane.input.mouse.pressed != button {
        return;
    }
    pane.input.mouse.pressed = None;
    if let Some(button) = button.filter(|_| pane.mouse_reporting()) {
        report_mouse(pane, button, ReportAction::Release, position, modifiers, cx);
    }
}

/// Adds key and mouse listeners to a pane's root.
pub fn attach(
    element: Stateful<Div>,
    pane: &Entity<TerminalPane>,
    _window: &mut Window,
    _cx: &mut App,
) -> Stateful<Div> {
    let keys = pane.clone();
    let down = pane.clone();
    let moved = pane.clone();
    let wheel = pane.clone();
    let mut element = element
        .key_context(KEY_CONTEXT)
        .on_key_down(move |event, window, cx| on_key_down(&keys, event, window, cx))
        .on_any_mouse_down(move |event, _, cx| {
            down.update(cx, |pane, cx| {
                let Some(button) = report_button(event.button) else {
                    return;
                };
                if !claims_press(pane, event.button, &event.modifiers) {
                    return;
                }
                pane.input.mouse.pressed = Some(button);
                report_mouse(
                    pane,
                    button,
                    ReportAction::Press,
                    event.position,
                    event.modifiers,
                    cx,
                );
            });
        })
        .on_mouse_move(move |event, _, cx| {
            moved.update(cx, |pane, cx| {
                // A drag is reported from `paint`, wherever the pointer goes.
                // A pane or tab being dragged is not a pointer over the program.
                if pane.input.mouse.pressed.is_none()
                    && pane.mouse_reporting()
                    && !crate::dragdrop::active(cx)
                {
                    report_motion(pane, event.position, event.modifiers, cx);
                }
            });
        })
        .on_scroll(move |event, _, cx| {
            let claimed = wheel.update(cx, |pane, cx| {
                if !pane.mouse_reporting() {
                    return false;
                }
                let Some(painted) = pane.painted_at() else {
                    return false;
                };
                let lines = match event.delta {
                    ScrollDelta::Lines(delta) => delta.y,
                    ScrollDelta::Pixels(delta) => {
                        f32::from(delta.y) / f32::from(painted.cell.height)
                    }
                };
                // One report per event that crossed a line, as xterm.js sent.
                let travel = pane.input.mouse.wheel + lines;
                pane.input.mouse.wheel = travel.fract();
                if travel.trunc() != 0. {
                    let button = if travel > 0. {
                        ReportButton::WheelUp
                    } else {
                        ReportButton::WheelDown
                    };
                    report_mouse(
                        pane,
                        button,
                        ReportAction::Press,
                        event.position,
                        event.modifiers,
                        cx,
                    );
                }
                true
            });
            if claimed {
                cx.stop_propagation();
            }
        });
    // A release outside the pane still ends what the program saw start.
    for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
        let inside = pane.clone();
        let outside = pane.clone();
        element = element
            .on_mouse_up(button, move |event, _, cx| {
                inside.update(cx, |pane, cx| {
                    let button = report_button(event.button);
                    release_mouse(pane, button, event.position, event.modifiers, cx);
                });
            })
            .on_mouse_up_out(button, move |event, _, cx| {
                outside.update(cx, |pane, cx| {
                    let button = report_button(event.button);
                    release_mouse(pane, button, event.position, event.modifiers, cx);
                });
            });
    }
    element
}

/// Runs during the pane's paint, with the element's bounds: registers the pane
/// as the window's text input while it has focus, and samples the paint half of
/// a keystroke's wait.
pub fn paint(
    pane: &Entity<TerminalPane>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let (focus, dirty_since, last_sample) = {
        let pane = pane.read(cx);
        (
            pane.focus_handle(cx),
            pane.dirty_since.get(),
            pane.input.last_paint_sample.get(),
        )
    };
    window.handle_input(&focus, ElementInputHandler::new(bounds, pane.clone()), cx);
    // A drag the program saw start is reported outside the pane too, clamped
    // to the grid, as xterm.js did: the pane's own move listener only hears the
    // pointer while it is over the pane. A move with no button held means the
    // release never reached the pane (it was hidden, say), so it ends the press
    // rather than drag on with no button down.
    let dragged = pane.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && dragged.read(cx).input.mouse.pressed.is_some() {
            dragged.update(cx, |pane, cx| {
                if event.pressed_button.is_some() {
                    report_motion(pane, event.position, event.modifiers, cx);
                } else {
                    let pressed = pane.input.mouse.pressed;
                    release_mouse(pane, pressed, event.position, event.modifiers, cx);
                }
            });
        }
    });
    let now = Instant::now();
    if let Some(since) = dirty_since
        && last_sample.is_none_or(|at| now.saturating_duration_since(at) >= PAINT_SAMPLE_INTERVAL)
    {
        pane.read(cx).input.last_paint_sample.set(Some(now));
        InputLatency::global(cx).sample(Segment::Paint, now.saturating_duration_since(since));
    }
}

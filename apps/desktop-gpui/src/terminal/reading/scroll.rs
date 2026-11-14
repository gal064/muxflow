//! Scrollback: the wheel, the scrollbar, and the two facts the engine needs
//! from them. Leaving the bottom starts reading; reaching the top of what this
//! terminal holds asks tmux for the page above it (`onScrollbackTopReached`).

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::TermMode;
use gpui_kit::{Context, ScrollDelta};
use terminal_engine::{Command, HistoryPageTrigger};

use super::super::pane::TerminalPane;

/// Rows per line GPUI reports. Tauri's xterm.js scrolled 50 px a wheel notch,
/// about 3 rows at the default font. GPUI's Linux and Windows backends already
/// report a notch as 3 lines; macOS reports the notch itself, 1 line.
const ROWS_PER_LINE: f32 = if cfg!(target_os = "macos") { 3. } else { 1. };

/// Trackpad pixels as Tauri scrolled them: xterm.js moves 50 px a tick. On
/// macOS WebKit counts 40 px as a tick (`pixelsPerLineStep`); on Wayland GTK
/// counts 10 axis units as a tick, and GPUI has already tripled each unit.
/// Only macOS and Wayland report pixels.
const PIXEL_SCALE: f32 = if cfg!(target_os = "macos") {
    50. / 40.
} else {
    50. / 10. / 3.
};

/// How far a wheel or trackpad event scrolls, in rows. Positive is up, into
/// the history.
pub fn wheel_rows(delta: ScrollDelta, cell_height: f32) -> f32 {
    match delta {
        ScrollDelta::Lines(lines) => lines.y * ROWS_PER_LINE,
        ScrollDelta::Pixels(_) if cell_height <= 0. => 0.,
        ScrollDelta::Pixels(pixels) => f32::from(pixels.y) * PIXEL_SCALE / cell_height,
    }
}

/// Whole rows to move now, carrying the rest to the next event. Rounded, as
/// xterm.js shows the row nearest its scroll position.
pub fn wheel_lines(carry: &mut f32, rows: f32) -> i32 {
    *carry += rows;
    let lines = carry.round();
    *carry -= lines;
    lines as i32
}

/// What the wheel sends a full-screen program that does not read the mouse:
/// cursor keys, as xterm does on a buffer with no scrollback.
pub fn alternate_scroll_bytes(lines: i32, application_cursor: bool) -> Vec<u8> {
    let key: &[u8] = match (application_cursor, lines > 0) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1bOB",
        (false, true) => b"\x1b[A",
        (false, false) => b"\x1b[B",
    };
    key.repeat(lines.unsigned_abs() as usize)
}

/// The scrollbar thumb's top and height in a track `track` long, or `None`
/// when there is nothing above the screen.
pub fn thumb(
    history: usize,
    offset: usize,
    rows: usize,
    track: f32,
    min: f32,
) -> Option<(f32, f32)> {
    if history == 0 || track <= 0. {
        return None;
    }
    let total = (history + rows) as f32;
    let height = (track * rows as f32 / total).max(min).min(track);
    let scrolled = (history - offset.min(history)) as f32 / history as f32;
    Some(((track - height) * scrolled, height))
}

/// The display offset that puts the thumb's middle at `y` in the track.
pub fn offset_at(history: usize, rows: usize, track: f32, min: f32, y: f32) -> usize {
    let Some((_, height)) = thumb(history, 0, rows, track, min) else {
        return 0;
    };
    let room = track - height;
    if room <= 0. {
        return 0;
    }
    let scrolled = ((y - height / 2.) / room).clamp(0., 1.);
    history - (scrolled * history as f32).round() as usize
}

/// Moves the viewport `lines` up (negative is down) and tells the engine.
pub fn scroll(pane: &TerminalPane, lines: i32, cx: &mut Context<TerminalPane>) {
    let Some(terminal) = pane.terminal() else {
        return;
    };
    let (at_bottom, at_top) = {
        let mut term = terminal.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        term.scroll_display(Scroll::Delta(lines));
        let offset = term.grid().display_offset();
        (offset == 0, offset == term.grid().history_size())
    };
    let link = pane.link().read(cx);
    let pane_id = pane.pane_id().to_string();
    link.send(Command::Viewport {
        pane_id: pane_id.clone(),
        at_bottom,
    });
    // Pushing up against the top is the request, whether or not the view
    // moved: a pane seeded with one screen has nothing to scroll yet.
    if lines > 0 && at_top {
        link.send(Command::RequestPage {
            pane_id,
            trigger: HistoryPageTrigger::ScrolledToTop,
        });
    }
    cx.notify();
}

/// Moves the viewport so its offset from the bottom is `offset`.
pub fn scroll_to(pane: &TerminalPane, offset: usize, cx: &mut Context<TerminalPane>) {
    let Some(current) = pane
        .terminal()
        .map(|terminal| terminal.lock().grid().display_offset())
    else {
        return;
    };
    let delta = offset as i32 - current as i32;
    if delta != 0 {
        scroll(pane, delta, cx);
    }
}

/// The offset that shows `line` mid-screen, as xterm's search does for a
/// match that is off screen. `None` when it is already on screen.
pub fn offset_showing<T>(term: &alacritty_terminal::Term<T>, line: i32) -> Option<usize> {
    let offset = term.grid().display_offset() as i32;
    let rows = term.screen_lines() as i32;
    let top = -offset;
    if (top..top + rows).contains(&line) {
        return None;
    }
    let wanted = -(line - rows / 2);
    Some(wanted.clamp(0, term.grid().history_size() as i32) as usize)
}

#[cfg(test)]
mod tests {
    use super::super::buffer::tests::term;
    use super::*;

    use gpui_kit::{point, px};

    /// One notch as this platform's GPUI backend reports it.
    const NOTCH: f32 = if cfg!(target_os = "macos") { 1. } else { 3. };

    fn lines(y: f32) -> ScrollDelta {
        ScrollDelta::Lines(point(0., y))
    }

    fn pixels(y: f32) -> ScrollDelta {
        ScrollDelta::Pixels(point(px(0.), px(y)))
    }

    fn moved(carry: &mut f32, events: impl IntoIterator<Item = ScrollDelta>, cell: f32) -> i32 {
        events
            .into_iter()
            .map(|delta| wheel_lines(carry, wheel_rows(delta, cell)))
            .sum()
    }

    #[test]
    fn a_notch_moves_three_rows_as_it_did_in_tauri() {
        let mut carry = 0.;
        assert_eq!(moved(&mut carry, [lines(NOTCH)], 18.), 3);
        assert_eq!(moved(&mut carry, [lines(-NOTCH)], 18.), -3);
        // Whole rows a notch, so a burst never drifts.
        assert_eq!(moved(&mut carry, (0..30).map(|_| lines(NOTCH)), 18.), 90);
    }

    #[test]
    fn a_trackpad_tick_scrolls_what_xterm_scrolled_for_it() {
        // A tick is 40 px of macOS travel, or 10 Wayland axis units, which
        // GPUI reports as 30 px; xterm.js scrolled 50 px for it.
        let tick = if cfg!(target_os = "macos") { 40. } else { 30. };
        let close = |rows: f32, expected: f32| (rows - expected).abs() < 1e-4;
        assert!(close(wheel_rows(pixels(tick), 50.), 1.));
        assert!(close(wheel_rows(pixels(-tick), 25.), -2.));
        assert_eq!(wheel_rows(pixels(tick), 0.), 0.);
    }

    #[test]
    fn a_slow_drag_and_its_momentum_add_up_from_remainders() {
        let mut carry = 0.;
        // 200 events of a twentieth of a row each, far below one row.
        let step = 17. / 20. / PIXEL_SCALE;
        assert_eq!(moved(&mut carry, (0..200).map(|_| pixels(step)), 17.), 10);
        assert_eq!(moved(&mut carry, (0..200).map(|_| pixels(-step)), 17.), -10);
    }

    #[test]
    fn the_view_shows_the_row_nearest_the_scroll_position_as_xterm_does() {
        let mut carry = 0.;
        // At 0.45, 0.55, 0.45 and 0 rows xterm.js shows rows 0, 1, 0 and 0.
        assert_eq!(wheel_lines(&mut carry, 0.45), 0);
        assert_eq!(wheel_lines(&mut carry, 0.1), 1);
        assert_eq!(wheel_lines(&mut carry, -0.1), -1);
        assert_eq!(wheel_lines(&mut carry, -0.45), 0);
    }

    #[test]
    fn a_notch_on_a_full_screen_program_sends_a_key_per_row() {
        let mut carry = 0.;
        let rows = moved(&mut carry, [lines(NOTCH)], 18.);
        assert_eq!(alternate_scroll_bytes(rows, false), b"\x1b[A".repeat(3));
    }

    #[test]
    fn the_wheel_on_a_full_screen_program_sends_cursor_keys() {
        assert_eq!(alternate_scroll_bytes(2, false), b"\x1b[A\x1b[A");
        assert_eq!(alternate_scroll_bytes(-1, true), b"\x1bOB");
    }

    #[test]
    fn the_thumb_shows_the_screen_against_the_whole_history() {
        assert_eq!(thumb(0, 0, 24, 400., 20.), None);
        // 75 rows of history and 25 on screen: a quarter of the track.
        assert_eq!(thumb(75, 0, 25, 400., 20.), Some((300., 100.)));
        assert_eq!(thumb(75, 75, 25, 400., 20.), Some((0., 100.)));
        let (_, height) = thumb(10_000, 0, 24, 400., 20.).unwrap();
        assert_eq!(height, 20., "never too small to grab");
    }

    #[test]
    fn a_pointer_on_the_track_scrolls_to_where_the_thumb_would_be() {
        assert_eq!(offset_at(75, 25, 400., 20., 50.), 75);
        assert_eq!(offset_at(75, 25, 400., 20., 350.), 0);
        assert_eq!(offset_at(75, 25, 400., 20., 200.), 37);
        assert_eq!(offset_at(75, 25, 400., 20., -40.), 75);
    }

    #[test]
    fn an_off_screen_line_is_brought_to_the_middle() {
        let term = term(10, 4, "x\r\n".repeat(30).as_bytes());
        let history = term.grid().history_size() as i32;
        assert_eq!(offset_showing(&term, 1), None);
        assert_eq!(offset_showing(&term, -10), Some(12));
        assert_eq!(
            offset_showing(&term, -history),
            Some(history as usize),
            "clamped to the top"
        );
    }
}

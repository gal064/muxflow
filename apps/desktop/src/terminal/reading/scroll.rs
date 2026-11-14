//! Scrollback: the wheel, the scrollbar, and the two facts the engine needs
//! from them. Leaving the bottom starts reading; reaching the top of what this
//! terminal holds asks tmux for the page above it (`onScrollbackTopReached`).

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::TermMode;
use gpui_kit::Context;
use terminal_engine::{Command, HistoryPageTrigger};

use super::super::pane::TerminalPane;

/// Whole lines a wheel event moves, carrying the fraction a touchpad leaves
/// over to the next event. Positive is up, into the history.
pub fn wheel_lines(carry: &mut f32, pixels: f32, cell_height: f32) -> i32 {
    if cell_height <= 0. {
        return 0;
    }
    *carry += pixels / cell_height;
    let lines = carry.trunc();
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

    #[test]
    fn a_wheel_moves_whole_lines_and_keeps_the_fraction() {
        let mut carry = 0.;
        assert_eq!(wheel_lines(&mut carry, 3. * 17., 17.), 3);
        assert_eq!(wheel_lines(&mut carry, 10., 17.), 0);
        assert_eq!(wheel_lines(&mut carry, 10., 17.), 1);
        assert_eq!(wheel_lines(&mut carry, -51., 17.), -2);
        assert_eq!(wheel_lines(&mut carry, 5., 0.), 0);
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

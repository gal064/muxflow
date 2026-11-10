//! One `alacritty_terminal` per pane.
//!
//! The engine thread is the only writer. The UI takes the same lock to paint, so
//! a write holds it for a bounded slice of bytes rather than for a whole record.

use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Row, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, Processor, StdSyncHandler};

use crate::event::GridSize;
use crate::history_page::{compose_history_page, split_history_rows};

/// Rows kept above the screen, the same as the desktop's xterm.
pub const SCROLLBACK_LIMIT: usize = 10_000;

const WRITE_SLICE_BYTES: usize = 256 * 1024;
const RESET_TERMINAL: &[u8] = b"\x1bc";
const CLEAR_SCREEN: &[u8] = b"\x1b[2J";

/// `%output` carries the pane's raw bytes and tmux has already answered their
/// queries as the pane's terminal. Replying again would inject a duplicate into
/// the program's stdin, so the terminal's events go nowhere, except an OSC 52
/// clipboard write: tmux keeps that from a program only when its own
/// `set-clipboard` is `on`, so otherwise the desktop is the one to honor it.
#[derive(Clone, Copy, Default)]
pub struct Listener;

thread_local! {
    /// Where [`Listener`] leaves OSC 52 writes until [`PaneWriter::write`]
    /// collects them. Per thread rather than per terminal, so `Listener` stays a
    /// plain value; the engine parses on one thread and collects right after
    /// each write, so a write is never credited to another pane.
    static CLIPBOARD_WRITES: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::ClipboardStore(_, text) = event {
            CLIPBOARD_WRITES.with_borrow_mut(|writes| writes.push(text));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalFacts {
    pub scrollback_rows: usize,
    pub scrollback_limit: usize,
    pub grid: GridSize,
    pub alternate_screen: bool,
}

/// The size of the grid a page was asked against and how many rows it skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryPageAnchor {
    pub skip: usize,
    pub columns: usize,
    pub rows: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySpliceOutcome {
    Applied,
    Superseded,
    OverlapExceedsPage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GridOutcome {
    Applied(GridSize),
    Unchanged,
    Rejected(String),
}

/// The part of a pane the UI shares: the grid to paint and a repaint flag.
pub struct PaneTerminal {
    term: FairMutex<Term<Listener>>,
    dirty: AtomicBool,
}

impl std::fmt::Debug for PaneTerminal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PaneTerminal")
    }
}

impl PaneTerminal {
    pub fn lock(&self) -> impl DerefMut<Target = Term<Listener>> + '_ {
        self.term.lock()
    }

    /// Whether anything changed since the last call.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn facts(&self) -> TerminalFacts {
        let term = self.term.lock();
        TerminalFacts {
            scrollback_rows: term.grid().history_size(),
            scrollback_limit: SCROLLBACK_LIMIT,
            grid: GridSize {
                columns: term.columns(),
                rows: term.screen_lines(),
            },
            alternate_screen: term.mode().contains(TermMode::ALT_SCREEN),
        }
    }
}

fn new_term(columns: usize, rows: usize) -> Term<Listener> {
    let config = Config {
        scrolling_history: SCROLLBACK_LIMIT,
        // The desktop's xterm blinks a block cursor until a program says otherwise.
        default_cursor_style: CursorStyle {
            shape: CursorShape::Block,
            blinking: true,
        },
        ..Config::default()
    };
    Term::new(config, &GridSize { columns, rows }, Listener)
}

pub(crate) fn scratch_terminal(columns: usize, rows: usize, bytes: &[u8]) -> Term<Listener> {
    let mut term = new_term(columns, rows);
    Processor::<StdSyncHandler>::new().advance(&mut term, bytes);
    term
}

/// The rows a page occupies once a terminal has laid it out, oldest first.
fn page_rows(page: &[u8], columns: usize, rows: usize) -> Vec<Row<Cell>> {
    let mut scratch = scratch_terminal(columns, rows, page);
    let grid = scratch.grid_mut();
    let oldest = -(grid.history_size() as i32);
    let last = grid.cursor.point.line.0;
    (oldest..=last)
        .map(|line| std::mem::replace(&mut grid[Line(line)], Row::new(1)))
        .collect()
}

/// Where a reader's viewport is, as something a reflow cannot move: how many
/// hard line ends sit between the top of the screen and the cursor, and how far
/// into its logical line the top row was. Reflow keeps both true, and a row
/// number would not. Port of `resizeTerminalPreservingViewport`.
struct ViewportAnchor {
    hard_ends: usize,
    columns_into_line: usize,
}

fn row_is_wrapped(term: &Term<Listener>, line: i32) -> bool {
    let row = &term.grid()[Line(line)];
    row[Column(term.columns() - 1)]
        .flags
        .contains(Flags::WRAPLINE)
}

impl ViewportAnchor {
    fn capture(term: &Term<Listener>) -> Option<Self> {
        let display_offset = term.grid().display_offset() as i32;
        if display_offset == 0 || term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let top = -display_offset;
        let cursor = term.grid().cursor.point.line.0;
        let hard_ends = (top..cursor)
            .filter(|line| !row_is_wrapped(term, *line))
            .count();
        let oldest = -(term.grid().history_size() as i32);
        let mut columns_into_line = 0;
        let mut line = top - 1;
        while line >= oldest && row_is_wrapped(term, line) {
            columns_into_line += term.columns();
            line -= 1;
        }
        Some(Self {
            hard_ends,
            columns_into_line,
        })
    }

    fn restore(self, term: &mut Term<Listener>) {
        let cursor = term.grid().cursor.point.line.0;
        let oldest = -(term.grid().history_size() as i32);
        let mut hard_ends = 0;
        let mut top = cursor;
        while top > oldest && hard_ends <= self.hard_ends {
            top -= 1;
            if !row_is_wrapped(term, top) {
                hard_ends += 1;
            }
        }
        // `top` is the last row before the logical line the reader was in, or
        // the oldest row when the reflow pushed the line off the history.
        if hard_ends > self.hard_ends {
            top += 1;
        }
        let target = top + (self.columns_into_line / term.columns()) as i32;
        let target_offset = (-target).max(0);
        let current = term.grid().display_offset() as i32;
        term.scroll_display(Scroll::Delta(target_offset - current));
    }
}

/// The engine's side of a pane: the parser, which the UI never needs.
pub(crate) struct PaneWriter {
    shared: Arc<PaneTerminal>,
    processor: Processor<StdSyncHandler>,
    wake: bool,
    /// The start of a UTF-8 sequence the last write ended inside.
    partial_utf8: Vec<u8>,
    /// OSC 52 writes the program made, oldest first.
    clipboard_writes: Vec<String>,
}

/// How many bytes at the end of `bytes` are an unfinished UTF-8 sequence.
fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    for (from_end, byte) in bytes.iter().rev().take(3).enumerate() {
        let held = from_end + 1;
        if byte & 0xc0 == 0x80 {
            continue;
        }
        let needed = match byte {
            0xf0.. => 4,
            0xe0.. => 3,
            0xc0.. => 2,
            _ => 1,
        };
        return if needed > held { held } else { 0 };
    }
    0
}

impl PaneWriter {
    pub(crate) fn new(size: GridSize) -> Self {
        Self {
            shared: Arc::new(PaneTerminal {
                term: FairMutex::new(new_term(size.columns.max(2), size.rows.max(2))),
                dirty: AtomicBool::new(false),
            }),
            processor: Processor::new(),
            wake: false,
            partial_utf8: Vec::new(),
            clipboard_writes: Vec::new(),
        }
    }

    /// The OSC 52 writes since the last call.
    pub(crate) fn take_clipboard_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.clipboard_writes)
    }

    pub(crate) fn terminal(&self) -> &Arc<PaneTerminal> {
        &self.shared
    }

    pub(crate) fn facts(&self) -> TerminalFacts {
        self.shared.facts()
    }

    fn mark_dirty(&mut self) {
        if !self.shared.dirty.swap(true, Ordering::AcqRel) {
            self.wake = true;
        }
    }

    /// Whether the dirty flag went up since the last call, so a UI waiting on it
    /// is woken once however many writes one command made.
    pub(crate) fn take_wake(&mut self) -> bool {
        std::mem::take(&mut self.wake)
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        self.shared.term.lock().scroll_display(Scroll::Bottom);
        self.mark_dirty();
    }

    /// tmux cuts `%output` anywhere. vte 0.15 loses the character after a sequence
    /// completed across two `advance` calls when another multibyte one follows, so
    /// no call here ends inside a sequence.
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        let joined;
        let bytes = if self.partial_utf8.is_empty() {
            bytes
        } else {
            let mut held = std::mem::take(&mut self.partial_utf8);
            held.extend_from_slice(bytes);
            joined = held;
            &joined
        };
        let (mut ready, tail) = bytes.split_at(bytes.len() - incomplete_utf8_tail(bytes));
        self.partial_utf8 = tail.to_vec();
        if ready.is_empty() {
            return;
        }
        // Anything a scratch parse left behind is not this pane's.
        CLIPBOARD_WRITES.with_borrow_mut(Vec::clear);
        while !ready.is_empty() {
            let end = ready.len().min(WRITE_SLICE_BYTES);
            let (slice, rest) = ready.split_at(end - incomplete_utf8_tail(&ready[..end]));
            let mut term = self.shared.term.lock();
            self.processor.advance(&mut *term, slice);
            drop(term);
            ready = rest;
        }
        CLIPBOARD_WRITES.with_borrow_mut(|writes| self.clipboard_writes.append(writes));
        self.mark_dirty();
    }

    /// Replaces the terminal's state with `bytes`. The parser goes too: a
    /// sequence or synchronized update cut off by the reset must not continue
    /// into the new screen.
    pub(crate) fn seed(&mut self, bytes: &[u8]) {
        self.processor = Processor::new();
        self.partial_utf8.clear();
        {
            let mut term = self.shared.term.lock();
            self.processor.advance(&mut *term, RESET_TERMINAL);
        }
        // The host opens every seed by clearing a screen the reset already cleared,
        // and alacritty counts that as a scrolled-off row. A seed has nothing above
        // it, and the pager counts those rows to ask tmux what to skip.
        self.write(bytes.strip_prefix(CLEAR_SCREEN).unwrap_or(bytes));
        self.mark_dirty();
    }

    pub(crate) fn sync_deadline(&self) -> Option<Instant> {
        self.processor.sync_timeout().sync_timeout()
    }

    /// Applies bytes a program held back in a synchronized update.
    pub(crate) fn flush_sync(&mut self) {
        if self.processor.sync_bytes_count() == 0 && self.sync_deadline().is_none() {
            return;
        }
        // The held bytes may hold an OSC 52 write; it is this pane's as much
        // as one `write` parses.
        CLIPBOARD_WRITES.with_borrow_mut(Vec::clear);
        let mut term = self.shared.term.lock();
        self.processor.stop_sync(&mut *term);
        drop(term);
        CLIPBOARD_WRITES.with_borrow_mut(|writes| self.clipboard_writes.append(writes));
        self.mark_dirty();
    }

    pub(crate) fn set_grid(&mut self, size: GridSize) -> GridOutcome {
        let GridSize { columns, rows } = size;
        if columns < 2 || rows < 2 {
            return GridOutcome::Rejected(format!(
                "{columns}x{rows} is not a usable terminal grid"
            ));
        }
        if self.facts().grid == size {
            return GridOutcome::Unchanged;
        }
        // Bytes a synchronized update is holding were written for the old grid.
        self.flush_sync();
        {
            let mut term = self.shared.term.lock();
            let anchor = ViewportAnchor::capture(&term);
            term.resize(size);
            if let Some(anchor) = anchor {
                anchor.restore(&mut term);
            }
        }
        self.mark_dirty();
        GridOutcome::Applied(size)
    }

    /// Puts a page of history above the oldest row this terminal holds.
    ///
    /// The refusals are `prependHistory`'s. Rows go in place and nothing above
    /// the page moves relative to the bottom, so a user reading scrollback stays
    /// on the same rows without any position being saved and restored.
    pub(crate) fn prepend_history(
        &mut self,
        history: &[u8],
        anchor: HistoryPageAnchor,
    ) -> HistorySpliceOutcome {
        // The bytes a synchronized update is holding are rows tmux's capture
        // already counted.
        self.flush_sync();
        let facts = self.facts();
        if facts.alternate_screen
            || facts.scrollback_rows >= facts.scrollback_limit
            || facts.grid.columns != anchor.columns
            || facts.grid.rows != anchor.rows
        {
            return HistorySpliceOutcome::Superseded;
        }
        let captured = split_history_rows(history);
        // What the ordered stream printed since the request is at the foot of
        // the page, and comes off it.
        let overlap = facts.scrollback_rows.saturating_sub(anchor.skip);
        if overlap >= captured.len() {
            return HistorySpliceOutcome::OverlapExceedsPage;
        }
        let kept = &captured[..captured.len() - overlap];
        let page = compose_history_page(kept, anchor.columns);
        let mut rows = page_rows(&page, anchor.columns, anchor.rows);
        let room = facts.scrollback_limit - facts.scrollback_rows;
        if rows.len() > room {
            rows.drain(..rows.len() - room);
        }
        let added = rows.len();
        {
            let mut term = self.shared.term.lock();
            let grid = term.grid_mut();
            let screen = grid.screen_lines() as i32;
            let history_before = grid.history_size() as i32;
            let shift = added as i32;
            // A whole-screen scroll is the public way to grow the history. It
            // moves every row up by `shift`, and the loop puts them back.
            grid.scroll_up::<Color>(&(Line(0)..Line(screen)), added);
            for line in (-history_before..screen).rev() {
                grid[Line(line)] = std::mem::replace(&mut grid[Line(line - shift)], Row::new(1));
            }
            for (offset, row) in rows.into_iter().enumerate() {
                grid[Line(-history_before - shift + offset as i32)] = row;
            }
            if grid.display_offset() != 0 {
                grid.scroll_display(Scroll::Delta(-shift));
            }
        }
        self.mark_dirty();
        HistorySpliceOutcome::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: GridSize = GridSize {
        columns: 8,
        rows: 3,
    };

    fn line_text(term: &Term<Listener>, line: i32) -> String {
        let row = &term.grid()[Line(line)];
        let text: String = (0..term.columns())
            .map(|column| row[Column(column)].c)
            .collect();
        text.trim_end().to_owned()
    }

    fn all_text(writer: &PaneWriter) -> Vec<String> {
        let term = writer.terminal().lock();
        let oldest = -(term.grid().history_size() as i32);
        (oldest..term.screen_lines() as i32)
            .map(|line| line_text(&term, line))
            .collect()
    }

    fn writer_with(lines: &[&str]) -> PaneWriter {
        let mut writer = PaneWriter::new(SIZE);
        writer.seed(lines.join("\r\n").as_bytes());
        writer
    }

    fn anchor(skip: usize) -> HistoryPageAnchor {
        HistoryPageAnchor {
            skip,
            columns: SIZE.columns,
            rows: SIZE.rows,
        }
    }

    #[test]
    fn puts_a_page_above_the_oldest_row_and_leaves_the_screen_alone() {
        let mut writer = writer_with(&["a", "b", "c", "d"]);
        assert_eq!(writer.facts().scrollback_rows, 1);
        let outcome = writer.prepend_history(b"x\r\ny", anchor(1));
        assert_eq!(outcome, HistorySpliceOutcome::Applied);
        assert_eq!(all_text(&writer), ["x", "y", "a", "b", "c", "d"]);
        assert_eq!(writer.facts().scrollback_rows, 3);
    }

    #[test]
    fn keeps_a_row_the_page_wrapped_as_one_logical_line() {
        let mut writer = writer_with(&["a"]);
        writer.prepend_history(b"abcdefgh\r\nij", anchor(0));
        let term = writer.terminal().lock();
        assert!(
            term.grid()[Line(-2)][Column(7)]
                .flags
                .contains(Flags::WRAPLINE)
        );
        assert_eq!(line_text(&term, -1), "ij");
        assert_eq!(line_text(&term, 0), "a");
    }

    #[test]
    fn keeps_a_reader_on_the_rows_they_were_reading() {
        let mut writer = writer_with(&["a", "b", "c", "d", "e"]);
        writer.terminal().lock().scroll_display(Scroll::Delta(1));
        let before = writer.terminal().lock().grid().display_offset();
        writer.prepend_history(b"x\r\ny", anchor(2));
        let term = writer.terminal().lock();
        assert_eq!(term.grid().display_offset(), before);
        assert_eq!(line_text(&term, -1), "b");
    }

    #[test]
    fn leaves_the_cursor_where_the_program_put_it() {
        let mut writer = writer_with(&["a", "b", "c", "d"]);
        let before = writer.terminal().lock().grid().cursor.point;
        writer.prepend_history(b"x", anchor(1));
        assert_eq!(writer.terminal().lock().grid().cursor.point, before);
    }

    #[test]
    fn trims_rows_printed_since_the_request_from_the_foot_of_the_page() {
        let mut writer = writer_with(&["a", "b", "c", "d"]);
        // Asked at skip 0, and one row has scrolled off since: the page's last
        // row is the one this buffer already holds.
        writer.prepend_history(b"x\r\ny\r\na", anchor(0));
        assert_eq!(all_text(&writer), ["x", "y", "a", "b", "c", "d"]);
    }

    #[test]
    fn says_so_when_every_row_of_the_page_was_already_printed() {
        let mut writer = writer_with(&["a", "b", "c", "d", "e"]);
        let outcome = writer.prepend_history(b"x\r\ny", anchor(0));
        assert_eq!(outcome, HistorySpliceOutcome::OverlapExceedsPage);
        assert_eq!(all_text(&writer), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn refuses_a_page_on_the_alternate_screen_or_at_another_grid() {
        let mut writer = writer_with(&["a"]);
        let mut other = anchor(0);
        other.columns = 9;
        assert_eq!(
            writer.prepend_history(b"x", other),
            HistorySpliceOutcome::Superseded
        );
        writer.write(b"\x1b[?1049h");
        assert_eq!(
            writer.prepend_history(b"x", anchor(0)),
            HistorySpliceOutcome::Superseded
        );
    }

    #[test]
    fn refuses_a_page_once_the_scrollback_is_full_and_truncates_one_that_would_overflow_it() {
        let mut writer = PaneWriter::new(SIZE);
        writer.seed("z\r\n".repeat(SCROLLBACK_LIMIT).as_bytes());
        let held = writer.facts().scrollback_rows;
        assert_eq!(held, SCROLLBACK_LIMIT - 2);
        let outcome = writer.prepend_history(b"old\r\nu\r\nv\r\nw", anchor(held));
        assert_eq!(outcome, HistorySpliceOutcome::Applied);
        assert_eq!(writer.facts().scrollback_rows, SCROLLBACK_LIMIT);
        assert_eq!(all_text(&writer)[..3], ["v", "w", "z"]);
        let held = writer.facts().scrollback_rows;
        assert_eq!(
            writer.prepend_history(b"x", anchor(held)),
            HistorySpliceOutcome::Superseded
        );
    }

    #[test]
    fn a_seed_replaces_the_scrollback_and_any_sequence_the_parser_was_inside() {
        let mut writer = writer_with(&["a", "b", "c", "d"]);
        writer.write(b"\x1b[?2026hheld");
        writer.seed(b"fresh");
        assert_eq!(writer.facts().scrollback_rows, 0);
        assert_eq!(all_text(&writer)[0], "fresh");
    }

    #[test]
    fn holds_a_synchronized_update_until_it_ends_or_is_flushed() {
        let mut writer = PaneWriter::new(SIZE);
        writer.write(b"\x1b[?2026hheld");
        assert_eq!(all_text(&writer)[0], "");
        assert!(writer.sync_deadline().is_some());
        writer.flush_sync();
        assert_eq!(all_text(&writer)[0], "held");
    }

    #[test]
    fn rejects_a_grid_nothing_can_be_laid_out_on_and_notices_when_nothing_changes() {
        let mut writer = PaneWriter::new(SIZE);
        assert!(matches!(
            writer.set_grid(GridSize {
                columns: 1,
                rows: 4
            }),
            GridOutcome::Rejected(_)
        ));
        assert_eq!(writer.set_grid(SIZE), GridOutcome::Unchanged);
        let bigger = GridSize {
            columns: 20,
            rows: 5,
        };
        assert_eq!(writer.set_grid(bigger), GridOutcome::Applied(bigger));
        assert_eq!(writer.facts().grid, bigger);
    }

    #[test]
    fn wakes_once_per_rise_of_the_dirty_flag_even_through_a_seed() {
        let mut writer = PaneWriter::new(SIZE);
        writer.seed(b"hello");
        assert!(writer.take_wake());
        writer.write(b"more");
        assert!(!writer.take_wake());
        assert!(writer.terminal().take_dirty());
        assert!(!writer.terminal().take_dirty());
        writer.write(b"");
        assert!(!writer.take_wake());
        writer.write(b"after");
        assert!(writer.take_wake());
    }

    #[test]
    fn applies_bytes_held_by_a_synchronized_update_before_a_resize() {
        let mut writer = PaneWriter::new(GridSize {
            columns: 40,
            rows: 3,
        });
        writer.write(b"\x1b[?2026h\x1b[1;30Hxy");
        writer.set_grid(GridSize {
            columns: 20,
            rows: 3,
        });
        writer.write(b"\x1b[?2026l");
        assert_eq!(writer.processor.sync_bytes_count(), 0);
        let rows = all_text(&writer);
        assert!(
            rows.iter().any(|row| row.contains("xy")),
            "the pair was split across rows: {rows:?}"
        );
    }

    fn reading_writer(columns: usize, rows: usize, lines: usize) -> PaneWriter {
        let mut writer = PaneWriter::new(GridSize { columns, rows });
        // Thirty printable cells fit at 40 columns and wrap at 20, so every row
        // before the viewport moves during the resize.
        let text: Vec<String> = (0..lines)
            .map(|index| format!("row-{index:02}-{}", "x".repeat(23)))
            .collect();
        writer.seed(text.join("\r\n").as_bytes());
        writer
    }

    fn top_line(writer: &PaneWriter) -> String {
        let term = writer.terminal().lock();
        line_text(&term, -(term.grid().display_offset() as i32))
    }

    #[test]
    fn a_reader_stays_on_their_row_through_a_reflow_and_back() {
        let mut writer = reading_writer(40, 5, 40);
        {
            let mut term = writer.terminal().lock();
            let history = term.grid().history_size() as i32;
            term.scroll_display(Scroll::Delta(history - 15));
        }
        assert!(top_line(&writer).starts_with("row-15"));
        writer.set_grid(GridSize {
            columns: 20,
            rows: 5,
        });
        assert!(
            top_line(&writer).starts_with("row-15"),
            "{}",
            top_line(&writer)
        );
        assert_ne!(writer.terminal().lock().grid().display_offset(), 0);
        writer.set_grid(GridSize {
            columns: 40,
            rows: 5,
        });
        assert!(
            top_line(&writer).starts_with("row-15"),
            "{}",
            top_line(&writer)
        );
    }

    #[test]
    fn a_reader_mid_wrap_keeps_their_share_of_the_line() {
        let mut writer = reading_writer(20, 5, 40);
        {
            let mut term = writer.terminal().lock();
            let target = 30 * 2 + 1;
            let history = term.grid().history_size() as i32;
            term.scroll_display(Scroll::Delta(history - target));
        }
        assert!(
            top_line(&writer).starts_with("xxx"),
            "{}",
            top_line(&writer)
        );
        writer.set_grid(GridSize {
            columns: 10,
            rows: 5,
        });
        // A logical line is 30 cells: its second 20-column row started at cell 20,
        // which is the third row at 10 columns.
        assert!(
            top_line(&writer).starts_with("xxx"),
            "{}",
            top_line(&writer)
        );
    }

    #[test]
    fn a_bottom_following_pane_stays_at_the_bottom_through_a_reflow() {
        let mut writer = reading_writer(40, 5, 40);
        writer.set_grid(GridSize {
            columns: 20,
            rows: 8,
        });
        let term = writer.terminal().lock();
        assert_eq!(term.grid().display_offset(), 0);
    }

    #[test]
    fn a_resize_on_the_alternate_screen_never_touches_the_viewport() {
        let mut writer = reading_writer(40, 5, 40);
        writer.write(b"\x1b[?1049h");
        writer.set_grid(GridSize {
            columns: 20,
            rows: 5,
        });
        assert_eq!(writer.terminal().lock().grid().display_offset(), 0);
    }

    #[test]
    fn keeps_every_character_when_a_frame_ends_inside_one() {
        let mut writer = PaneWriter::new(GridSize {
            columns: 20,
            rows: 3,
        });
        let text = "\u{e9} \u{4e2d}x".as_bytes();
        writer.write(&text[..1]);
        writer.write(&text[1..]);
        assert_eq!(all_text(&writer)[0], "\u{e9} \u{4e2d} x");
    }

    #[test]
    fn a_seed_forgets_a_character_the_old_stream_left_unfinished() {
        let mut writer = PaneWriter::new(GridSize {
            columns: 20,
            rows: 3,
        });
        writer.write(&"\u{e9}".as_bytes()[..1]);
        writer.seed(b"fresh");
        writer.write(b"!");
        assert_eq!(all_text(&writer)[0], "fresh!");
    }

    #[test]
    fn a_seed_in_the_hosts_shape_has_no_scrollback() {
        let mut writer = PaneWriter::new(SIZE);
        writer.seed(b"\x1b[2J\x1b[Hscreen");
        assert_eq!(writer.facts().scrollback_rows, 0);
        assert_eq!(all_text(&writer)[0], "screen");
    }

    #[test]
    fn keeps_the_text_after_a_wide_joined_emoji_when_the_erase_comes_first() {
        let mut writer = PaneWriter::new(GridSize {
            columns: 20,
            rows: 3,
        });
        writer.seed("\x1b[2J\x1b[1;9H\x1b[K\x1b[m\x1b[H\u{1f469}\u{200d}\u{1f4bb} tail".as_bytes());
        assert!(all_text(&writer)[0].ends_with("tail"));
    }
}

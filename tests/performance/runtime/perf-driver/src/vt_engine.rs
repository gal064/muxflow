//! S9: does `alacritty_terminal` agree with tmux about what is on screen?
//!
//! The vt100 comparison in the parity lane says nothing about the engine the
//! GPUI terminal will use, and it renders tmux's capture through the same
//! parser it is checking, so a width disagreement cancels out. This module owns
//! the engine side:
//!
//! - replay delivered bytes through `alacritty_terminal` (the real `Term` and
//!   its `vte` processor, the same pair the renderer will drive);
//! - read `capture-pane -p -e` without any terminal parser, so tmux's answer is
//!   not filtered through the engine under test;
//! - the width fixture, whose rows carry a right-hand anchor at a fixed column,
//!   because a capture has no padding cells and a row's text alone cannot say
//!   which column a character landed in;
//! - the history-page check: a port of `composeHistoryPage` and the proof that
//!   the alacritty grid marks its continuations `WRAPLINE`.

use std::{
    io::{Read as _, Write as _},
    process::{Command, Stdio},
};

use alacritty_terminal::{
    event::VoidListener,
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{Config, Term, cell::Flags},
    vte::ansi::{Color, NamedColor, Processor},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::{row_text, truncate};

/// The 1-based column every anchored fixture row writes its anchor at, with an
/// absolute move, so tmux's anchor column is known without asking tmux.
pub const ANCHOR_COLUMN: usize = 60;
const ANCHOR: char = '|';

/// A diff list long enough to name every disagreement class, short enough to
/// read in the lane's JSON.
const DIFF_LIMIT: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// The attributes the existing lane gates on, in one shape for both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle {
    pub bold: bool,
    pub dim: bool,
    pub fg: CellColor,
    pub bg: CellColor,
}

const PLAIN: CellStyle = CellStyle {
    bold: false,
    dim: false,
    fg: CellColor::Default,
    bg: CellColor::Default,
};

/// One row as code points in screen order, each with its style. A row is
/// compared this way because the two sides store a cluster differently: tmux
/// keeps it in one cell, alacritty in a base cell plus zero-width code points.
pub type StyledRow = Vec<(char, CellStyle)>;

/// Cell count an `alacritty_terminal` grid is built with; the crate's own
/// `TermSize` lives in its test-support module.
#[derive(Clone, Copy)]
pub struct GridSize {
    pub rows: usize,
    pub columns: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// The screen those bytes produce in the engine.
pub fn replay(bytes: &[u8], rows: usize, columns: usize) -> Term<VoidListener> {
    let mut term = Term::new(Config::default(), &GridSize { rows, columns }, VoidListener);
    let mut processor: Processor = Processor::new();
    processor.advance(&mut term, bytes);
    // A frame still inside a DEC 2026 bracket when the log ends is held back by
    // the processor; the real engine flushes it on its timeout.
    if processor.sync_bytes_count() > 0 {
        processor.stop_sync(&mut term);
    }
    term
}

fn normalize_color(color: Color) -> CellColor {
    match color {
        Color::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => CellColor::Indexed(index),
        Color::Named(named) if (named as usize) < 16 => CellColor::Indexed(named as u8),
        // Foreground, Background and the renderer-derived dim and bright
        // defaults: an SGR can only reach them through "default".
        Color::Named(NamedColor::Foreground | NamedColor::Background) | Color::Named(_) => {
            CellColor::Default
        }
    }
}

/// Row `row` of the visible screen, as [`StyledRow`].
pub fn engine_row(term: &Term<VoidListener>, row: usize) -> StyledRow {
    let cells = &term.grid()[Line(row as i32)];
    let mut styled = Vec::new();
    for column in 0..term.grid().columns() {
        let cell = &cells[Column(column)];
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let style = CellStyle {
            bold: cell.flags.contains(Flags::BOLD),
            dim: cell.flags.contains(Flags::DIM),
            fg: normalize_color(cell.fg),
            bg: normalize_color(cell.bg),
        };
        styled.push((cell.c, style));
        for zero_width in cell.zerowidth().unwrap_or_default() {
            styled.push((*zero_width, style));
        }
    }
    trim_row(styled)
}

/// The column the anchor landed in, or `None` when the row has no anchor.
pub fn engine_anchor_column(term: &Term<VoidListener>, row: usize) -> Option<usize> {
    let cells = &term.grid()[Line(row as i32)];
    (0..term.grid().columns()).find(|column| cells[Column(*column)].c == ANCHOR)
}

/// The same, for the vt100 comparison parser.
pub fn vt100_anchor_column(screen: &vt100::Screen, row: u16, columns: u16) -> Option<usize> {
    (0..columns)
        .find(|column| {
            screen
                .cell(row, *column)
                .is_some_and(|cell| cell.contents() == ANCHOR.to_string())
        })
        .map(usize::from)
}

/// A capture has no trailing blanks, and an engine row has them up to the
/// margin. Only a blank that is really blank goes: a styled one is content.
fn trim_row(mut row: StyledRow) -> StyledRow {
    while row
        .last()
        .is_some_and(|(c, style)| *c == ' ' && *style == PLAIN)
    {
        row.pop();
    }
    row
}

fn apply_sgr(parameters: &str, style: &mut CellStyle) {
    // tmux writes colour arguments with `;`, and the ISO form with `:`. Both
    // are flattened; the empty colourspace field of `38:2::r:g:b` is dropped.
    let mut values = parameters
        .split([';', ':'])
        .filter(|value| !value.is_empty() || parameters.is_empty())
        .map(|value| value.parse::<u16>().unwrap_or(0));
    let extended = |values: &mut dyn Iterator<Item = u16>| match values.next() {
        Some(5) => values.next().map(|index| CellColor::Indexed(index as u8)),
        Some(2) => {
            let red = values.next()?;
            let green = values.next()?;
            let blue = values.next()?;
            Some(CellColor::Rgb(red as u8, green as u8, blue as u8))
        }
        _ => None,
    };
    while let Some(value) = values.next() {
        match value {
            0 => *style = PLAIN,
            1 => style.bold = true,
            2 => style.dim = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            30..=37 => style.fg = CellColor::Indexed((value - 30) as u8),
            38 => style.fg = extended(&mut values).unwrap_or(style.fg),
            39 => style.fg = CellColor::Default,
            40..=47 => style.bg = CellColor::Indexed((value - 40) as u8),
            48 => style.bg = extended(&mut values).unwrap_or(style.bg),
            49 => style.bg = CellColor::Default,
            90..=97 => style.fg = CellColor::Indexed((value - 90 + 8) as u8),
            100..=107 => style.bg = CellColor::Indexed((value - 100 + 8) as u8),
            _ => {}
        }
    }
}

/// tmux's screen, read straight off `capture-pane -p -e`: code points and SGR
/// state, with no terminal parser between tmux and the comparison.
pub fn parse_capture(capture: &[u8], rows: usize) -> Vec<StyledRow> {
    let mut parsed = Vec::with_capacity(rows);
    // Outside the row loop: tmux writes SGR only when it changes, so a run that
    // reaches the next row is not reopened there.
    let mut style = PLAIN;
    for line in capture.split(|byte| *byte == b'\n').take(rows) {
        let text = String::from_utf8_lossy(line);
        let mut chars = text.chars().peekable();
        let mut row = Vec::new();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                row.push((c, style));
                continue;
            }
            if chars.next_if_eq(&'[').is_none() {
                // tmux's capture writes only SGR; anything else is one
                // two-byte escape, and it is not a cell either way.
                chars.next();
                continue;
            }
            let mut parameters = String::new();
            let mut last = None;
            for parameter in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&parameter) {
                    last = Some(parameter);
                    break;
                }
                parameters.push(parameter);
            }
            if last == Some('m') {
                apply_sgr(&parameters, &mut style);
            }
        }
        parsed.push(trim_row(row));
    }
    parsed.resize(rows, Vec::new());
    parsed
}

pub fn row_string(row: &StyledRow) -> String {
    row.iter().map(|(c, _)| *c).collect()
}

fn hex(text: &str) -> String {
    text.bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One fixture row whose sample is width-sensitive.
pub struct WidthRow {
    pub class: &'static str,
    pub sample: &'static str,
    /// SGR opened before the sample and reset after it, so the attribute gate
    /// has something to be wrong about on the awkward glyphs.
    pub style: &'static str,
}

/// Rows in the order they are painted, row 0 at the top of the pane.
pub const WIDTH_ROWS: &[WidthRow] = &[
    WidthRow {
        class: "cjk",
        sample: "漢字かな混じり文",
        style: "\x1b[1;38;5;208m",
    },
    WidthRow {
        class: "cjk",
        sample: "ｶﾀｶﾅ漢x字ｱ",
        style: "",
    },
    WidthRow {
        class: "emoji",
        sample: "🚀🎉😊",
        style: "\x1b[48;5;236m",
    },
    WidthRow {
        class: "emoji-flag",
        sample: "🇯🇵🇺🇸",
        style: "",
    },
    WidthRow {
        class: "emoji-vs16",
        sample: "\u{2764}\u{fe0f}\u{2600}\u{fe0f}\u{2708}\u{fe0f}",
        style: "\x1b[2m",
    },
    WidthRow {
        class: "emoji-vs16",
        sample: "#\u{fe0f}\u{20e3}",
        style: "",
    },
    WidthRow {
        class: "emoji-zwj",
        sample: "\u{1f469}\u{200d}\u{1f4bb}",
        style: "\x1b[38;2;10;200;90m",
    },
    WidthRow {
        class: "emoji-zwj",
        sample: "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
        style: "",
    },
    WidthRow {
        class: "emoji-zwj",
        sample: "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
        style: "",
    },
    WidthRow {
        class: "combining",
        sample: "e\u{301}a\u{308}o\u{302}n\u{303}",
        style: "\x1b[1m",
    },
    WidthRow {
        class: "combining",
        sample: "a\u{300}\u{301}\u{302}\u{303}z",
        style: "",
    },
    WidthRow {
        class: "box",
        sample: "┌─┬─┐│├┼┤└┴┘═║╔╗╚╝",
        style: "\x1b[38;5;33m",
    },
];

/// The wide glyph that does not fit in the last column of its row: tmux and the
/// engine must both leave that column blank and carry the glyph down.
const MARGIN_GLYPH: &str = "漢";

/// Row of the margin case, and of the row its glyph wraps onto.
pub const MARGIN_ROW: usize = WIDTH_ROWS.len();
const STATUS_ROW: usize = WIDTH_ROWS.len() + 3;
pub const FIXTURE_FRAME_MARKER: &[u8] = b"WIDTH FRAME ";

fn painted_row(row: usize, width_row: &WidthRow) -> String {
    let reset = if width_row.style.is_empty() {
        ""
    } else {
        "\x1b[0m"
    };
    format!(
        "\x1b[{line};1H\x1b[2K[{class}] {style}{sample}{reset}\x1b[{ANCHOR_COLUMN}G{ANCHOR} tail",
        line = row + 1,
        class = width_row.class,
        style = width_row.style,
        sample = width_row.sample,
    )
}

fn margin_rows(columns: usize) -> String {
    let filler = "x".repeat(columns - "[margin] ".len() - 1);
    format!(
        "\x1b[{first};1H\x1b[2K\x1b[{second};1H\x1b[2K\x1b[{first};1H[margin] {filler}{MARGIN_GLYPH} tail",
        first = MARGIN_ROW + 1,
        second = MARGIN_ROW + 2,
    )
}

/// One complete repaint, ended by the frame counter the driver waits for.
pub fn fixture_frame(columns: usize, frame: u64) -> Vec<u8> {
    let mut painted = String::new();
    for (row, width_row) in WIDTH_ROWS.iter().enumerate() {
        painted.push_str(&painted_row(row, width_row));
    }
    painted.push_str(&margin_rows(columns));
    painted.push_str(&format!(
        "\x1b[{};1H\x1b[2K{}{frame}",
        STATUS_ROW + 1,
        String::from_utf8_lossy(FIXTURE_FRAME_MARKER),
    ));
    painted.into_bytes()
}

pub const HISTORY_START: &str = "HIST-START";
pub const HISTORY_END: &str = "HIST-END";

/// Lines that end up in tmux's scrollback, as the pane printed them: rows that
/// wrap (with an SGR run spanning the wrap), a row that does not, and a blank.
pub fn history_block(columns: usize) -> Vec<u8> {
    let mut block = String::new();
    block.push_str(&format!("{HISTORY_START}\r\n"));
    block.push_str(&"a".repeat(columns * 2 + 5));
    block.push_str("\r\nshort row\r\n");
    block.push_str(&format!(
        "\x1b[1m{}\x1b[22m{}\r\n",
        "b".repeat(columns + 10),
        "c".repeat(15)
    ));
    block.push_str("\r\n");
    block.push_str(&"d".repeat(columns + 3));
    block.push_str(&format!("\r\n{HISTORY_END}\r\n"));
    block.into_bytes()
}

/// The pane program behind the width window: pushes [`history_block`] into
/// tmux's scrollback, paints, and repaints once per byte read from the tty.
pub fn run_width_fixture() -> Result<()> {
    let output = Command::new("stty")
        .arg("size")
        .stdin(Stdio::inherit())
        .output()
        .context("read the pane size")?;
    let size = String::from_utf8(output.stdout).context("stty output")?;
    let mut parts = size.split_whitespace();
    let rows: usize = parts.next().context("rows")?.parse().context("rows")?;
    let columns: usize = parts
        .next()
        .context("columns")?
        .parse()
        .context("columns")?;

    let mut out = std::io::stdout().lock();
    out.write_all(b"\x1b[2J\x1b[H")?;
    out.write_all(&history_block(columns))?;
    // Scrolls the whole block off the screen and into the history.
    out.write_all("\r\n".repeat(rows).as_bytes())?;
    let mut frame = 1;
    out.write_all(&fixture_frame(columns, frame))?;
    out.flush()?;

    let mut stdin = std::io::stdin().lock();
    let mut key = [0_u8; 1];
    while stdin.read(&mut key)? == 1 {
        frame += 1;
        out.write_all(&fixture_frame(columns, frame))?;
        out.flush()?;
    }
    Ok(())
}

/// One replayed screen against tmux.
#[derive(Default)]
pub struct EngineParity {
    pub text_mismatch_rows: usize,
    pub attribute_mismatch_cells: usize,
    pub anchor_mismatch_rows: usize,
    pub diffs: Vec<Value>,
    pub attribute_diffs: Vec<String>,
    /// One verdict for each row of the width fixture.
    pub fixture_rows: Vec<FixtureRowVerdict>,
}

pub struct FixtureRowVerdict {
    pub row: usize,
    pub class: &'static str,
    pub engine_pass: bool,
    pub vt100_pass: bool,
}

impl EngineParity {
    pub fn passed(&self) -> bool {
        self.text_mismatch_rows == 0
            && self.attribute_mismatch_cells == 0
            && self.anchor_mismatch_rows == 0
    }

    pub fn to_json(&self) -> Value {
        json!({
            "textMismatchRows": self.text_mismatch_rows,
            "attributeMismatchCells": self.attribute_mismatch_cells,
            "anchorMismatchRows": self.anchor_mismatch_rows,
            "pass": self.passed(),
            "diffs": self.diffs,
            "attributeDiffs": self.attribute_diffs,
        })
    }
}

/// Which width-fixture sample a row of the pane holds.
fn fixture_row(row: usize) -> Option<&'static WidthRow> {
    WIDTH_ROWS.get(row)
}

/// The class of a row in the width pane, anchored or not.
fn fixture_class(row: usize) -> Option<&'static str> {
    if let Some(width_row) = fixture_row(row) {
        return Some(width_row.class);
    }
    (row == MARGIN_ROW || row == MARGIN_ROW + 1).then_some("margin")
}

/// Compares the engine's screen with tmux's. `width_fixture` adds the anchor
/// gate and the per-class accounting, and carries the vt100 screen for the same
/// bytes so a limit of the comparison parser is not blamed on the engine.
pub fn compare_screen(
    term: &Term<VoidListener>,
    truth: &[StyledRow],
    width_fixture: Option<&vt100::Screen>,
) -> EngineParity {
    let mut parity = EngineParity::default();
    let columns = term.grid().columns();
    for (row, truth_row) in truth.iter().enumerate() {
        let engine = engine_row(term, row);
        let engine_text = row_string(&engine);
        let truth_text = row_string(truth_row);
        let text_differs = engine_text != truth_text;
        if text_differs {
            parity.text_mismatch_rows += 1;
        }
        let mut attributes_differ = false;
        for (index, (engine_cell, truth_cell)) in engine.iter().zip(truth_row).enumerate() {
            if engine_cell.1 != truth_cell.1 {
                attributes_differ = true;
                parity.attribute_mismatch_cells += 1;
                if parity.attribute_diffs.len() < DIFF_LIMIT {
                    parity.attribute_diffs.push(format!(
                        "row {row} code point {index} {:?}: engine {:?} tmux {:?}",
                        engine_cell.0, engine_cell.1, truth_cell.1
                    ));
                }
            }
        }

        let anchored = width_fixture.is_some() && fixture_row(row).is_some();
        let anchor_column = anchored.then(|| engine_anchor_column(term, row)).flatten();
        let anchor_differs = anchored && anchor_column != Some(ANCHOR_COLUMN - 1);
        if anchor_differs {
            parity.anchor_mismatch_rows += 1;
        }
        let class = width_fixture.and(fixture_class(row));
        if let (Some(class), Some(screen)) = (class, width_fixture) {
            let vt100_text = row_text(screen, row as u16, columns as u16);
            let vt100_anchored = !anchored
                || vt100_anchor_column(screen, row as u16, columns as u16)
                    == Some(ANCHOR_COLUMN - 1);
            parity.fixture_rows.push(FixtureRowVerdict {
                row,
                class,
                engine_pass: !text_differs && !anchor_differs && !attributes_differ,
                vt100_pass: vt100_text == truth_text && vt100_anchored,
            });
        }

        if (text_differs || anchor_differs) && parity.diffs.len() < DIFF_LIMIT {
            let mut diff = json!({
                "row": row,
                "tmux": truncate(&truth_text),
                "engine": truncate(&engine_text),
            });
            if let Some(class) = class {
                diff["class"] = json!(class);
            }
            if let Some(width_row) = fixture_row(row).filter(|_| width_fixture.is_some()) {
                diff["sample"] = json!(width_row.sample);
                diff["sampleBytes"] = json!(hex(width_row.sample));
            }
            if anchored {
                diff["anchorColumnTmux"] = json!(ANCHOR_COLUMN - 1);
                diff["anchorColumnEngine"] = json!(anchor_column);
            }
            if let Some(screen) = width_fixture {
                let vt100_text = row_text(screen, row as u16, columns as u16);
                diff["vt100"] = json!(truncate(&vt100_text));
                diff["vt100MatchesTmux"] = json!(vt100_text == truth_text);
                if anchored {
                    diff["anchorColumnVt100"] =
                        json!(vt100_anchor_column(screen, row as u16, columns as u16));
                }
            }
            parity.diffs.push(diff);
        }
    }
    parity
}

// ---- history page ----------------------------------------------------------

/// `splitHistoryRows`: `n` separators mean `n + 1` rows, always.
pub fn split_history_rows(history: &[u8]) -> Vec<Vec<u8>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index + 1 < history.len() {
        if history[index] == 0x0d && history[index + 1] == 0x0a {
            rows.push(history[start..index].to_vec());
            index += 1;
            start = index + 1;
        }
        index += 1;
    }
    rows.push(history[start..].to_vec());
    rows
}

/// `afterEscape`: the index just past one escape sequence.
fn after_escape(chars: &[char], escape: usize) -> usize {
    match chars.get(escape + 1) {
        Some('[') => {
            let mut index = escape + 2;
            while index < chars.len() {
                let code = chars[index];
                index += 1;
                if ('\x40'..='\x7e').contains(&code) {
                    break;
                }
            }
            index
        }
        Some(']') => {
            let mut index = escape + 2;
            while index < chars.len() {
                if chars[index] == '\x07' {
                    return index + 1;
                }
                if chars[index] == '\x1b' && chars.get(index + 1) == Some(&'\\') {
                    return index + 2;
                }
                index += 1;
            }
            index
        }
        _ => escape + 2,
    }
}

/// `plainText`: the row with its escape sequences and control bytes taken out.
fn plain_text(row: &[u8]) -> String {
    let text = String::from_utf8_lossy(row);
    let chars: Vec<char> = text.chars().collect();
    let mut plain = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\x1b' {
            index = after_escape(&chars, index);
        } else if c < '\x20' || c == '\x7f' {
            index += 1;
        } else {
            plain.push(c);
            index += 1;
        }
    }
    plain
}

/// `visibleWidth`: grapheme clusters, not code points. A cell holds a cluster,
/// so this never over-counts a row into looking as though it filled the grid.
pub fn visible_width(row: &[u8]) -> usize {
    plain_text(row).graphemes(true).count()
}

/// `composeHistoryPage`: a row that fills the grid gets no line break, so the
/// engine's own autowrap carries the next row onto the continuation line.
pub fn compose_history_page(rows: &[Vec<u8>], columns: usize) -> Vec<u8> {
    let widths: Vec<usize> = rows.iter().map(|row| visible_width(row)).collect();
    let mut composed = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        composed.extend_from_slice(row);
        if index == rows.len() - 1 {
            break;
        }
        let wraps_into_next = columns > 0 && widths[index] >= columns && widths[index + 1] > 0;
        if !wraps_into_next {
            composed.extend_from_slice(b"\r\n");
        }
    }
    composed
}

/// Top to bottom over history and screen, blank rows at both ends dropped:
/// each row's text and whether it continues onto the next. Reflow moves rows
/// between history and screen depending on the cursor, so positions are not
/// comparable between two terminals but this sequence is.
fn row_sequence(term: &Term<VoidListener>) -> Vec<(String, bool)> {
    let grid = term.grid();
    let columns = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32;
    let mut rows: Vec<(String, bool)> = (top..bottom)
        .map(|line| {
            let cells = &grid[Line(line)];
            let mut text = String::new();
            for column in 0..columns {
                let cell = &cells[Column(column)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(cell.c);
                text.extend(cell.zerowidth().unwrap_or_default());
            }
            let wrapped = cells[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
            (text.trim_end().to_owned(), wrapped)
        })
        .collect();
    while rows.last().is_some_and(|(text, _)| text.is_empty()) {
        rows.pop();
    }
    let leading = rows.iter().take_while(|(text, _)| text.is_empty()).count();
    rows.drain(..leading);
    rows
}

/// What a user gets by selecting the whole terminal and copying.
fn copied_text(term: &Term<VoidListener>) -> String {
    let grid = term.grid();
    let start = Point::new(Line(-(grid.history_size() as i32)), Column(0));
    let end = Point::new(
        Line(grid.screen_lines() as i32 - 1),
        Column(grid.columns() - 1),
    );
    term.bounds_to_string(start, end).trim_end().to_owned()
}

/// The rows of a page from its first start marker to its last end marker.
fn marked_rows(page: &[u8]) -> Option<Vec<Vec<u8>>> {
    let rows = split_history_rows(page);
    let has = |row: &Vec<u8>, marker: &str| String::from_utf8_lossy(row).contains(marker);
    let start = rows.iter().position(|row| has(row, HISTORY_START))?;
    let end = rows.iter().rposition(|row| has(row, HISTORY_END))?;
    (start < end).then(|| rows[start..=end].to_vec())
}

/// The S9 wrap verdict for one page of real history. `live` is the bytes the
/// pane printed for the same rows: the control, since the contract is that a
/// composed page behaves like live output.
pub fn wrap_check(page: &[u8], live: &[u8], columns: usize) -> Value {
    let Some(rows) = marked_rows(page) else {
        return json!({
            "pass": false,
            "error": "the page does not contain the marked history block",
            "pageBytes": page.len(),
        });
    };
    let widths: Vec<usize> = rows.iter().map(|row| visible_width(row)).collect();
    // Room for the narrowest reflow, so nothing depends on the history size.
    let lines = rows.len() * 3 + 20;
    let mut composed = replay(&compose_history_page(&rows, columns), lines, columns);
    let mut control = replay(live, lines, columns);

    let mut wrong_marks = Vec::new();
    let mut marked = 0;
    let mut expected = 0;
    for row in 0..rows.len() {
        let wraps = row + 1 < rows.len() && widths[row] >= columns && widths[row + 1] > 0;
        let flagged = composed.grid()[Line(row as i32)][Column(columns - 1)]
            .flags
            .contains(Flags::WRAPLINE);
        expected += usize::from(wraps);
        marked += usize::from(flagged);
        if wraps != flagged {
            wrong_marks.push(json!({ "row": row, "expected": wraps, "flagged": flagged }));
        }
    }

    let mut checks = Vec::new();
    let mut compare =
        |width: usize, composed: &Term<VoidListener>, control: &Term<VoidListener>| {
            let composed_rows = row_sequence(composed);
            let control_rows = row_sequence(control);
            let copy_matches = copied_text(composed) == copied_text(control);
            let matches = composed_rows == control_rows;
            let first_difference = composed_rows
                .iter()
                .zip(&control_rows)
                .position(|(left, right)| left != right);
            checks.push(json!({
                "columns": width,
                "rowsMatchLive": matches,
                "copyMatchesLive": copy_matches,
                "wrappedRows": composed_rows.iter().filter(|(_, wrapped)| *wrapped).count(),
                "firstDifference": first_difference.map(|index| json!({
                    "composed": composed_rows[index],
                    "live": control_rows[index],
                })),
            }));
            matches && copy_matches
        };

    // Each wrapped run of rows has to come back from a copy as one line.
    let copy = copied_text(&composed);
    let copied_lines: Vec<&str> = copy.lines().collect();
    let mut copy_joined = true;
    let mut logical = String::new();
    for (row, cells) in rows.iter().enumerate() {
        logical.push_str(&plain_text(cells));
        let continues = row + 1 < rows.len() && widths[row] >= columns && widths[row + 1] > 0;
        if !continues {
            copy_joined &= copied_lines.contains(&logical.as_str());
            logical.clear();
        }
    }
    let mut pass = wrong_marks.is_empty() && expected > 0 && copy_joined;
    pass &= compare(columns, &composed, &control);
    for width in [columns - 17, columns + 13] {
        let size = GridSize {
            rows: lines,
            columns: width,
        };
        composed.resize(size);
        control.resize(size);
        pass &= compare(width, &composed, &control);
    }
    json!({
        "pass": pass,
        "columns": columns,
        "pageRows": rows.len(),
        "continuationsExpected": expected,
        "continuationsMarked": marked,
        "wrongMarks": wrong_marks,
        "copyJoinsWrappedRows": copy_joined,
        "widthChecks": checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMNS: usize = 20;

    fn row(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    fn flagged(term: &Term<VoidListener>, line: usize, columns: usize) -> bool {
        term.grid()[Line(line as i32)][Column(columns - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    #[test]
    fn a_full_row_with_no_break_marks_the_continuation() {
        let page = compose_history_page(&[row(&"a".repeat(COLUMNS)), row("rest")], COLUMNS);
        assert_eq!(page.len(), COLUMNS + 4, "no separator may be written");
        let term = replay(&page, 6, COLUMNS);
        assert!(flagged(&term, 0, COLUMNS));
        assert_eq!(row_string(&engine_row(&term, 1)), "rest");
    }

    #[test]
    fn a_short_row_is_a_hard_break() {
        let page = compose_history_page(&[row("short"), row("next")], COLUMNS);
        let term = replay(&page, 6, COLUMNS);
        assert!(!flagged(&term, 0, COLUMNS));
    }

    /// The control that makes the check able to fail: the same rows written
    /// the way `-J`-less scrollback looks without the composer.
    #[test]
    fn hard_broken_rows_are_not_marked() {
        let mut page = "a".repeat(COLUMNS).into_bytes();
        page.extend_from_slice(b"\r\nrest");
        let term = replay(&page, 6, COLUMNS);
        assert!(!flagged(&term, 0, COLUMNS));
        assert_eq!(copied_text(&term), format!("{}\nrest", "a".repeat(COLUMNS)));
    }

    #[test]
    fn a_full_row_before_a_blank_row_keeps_its_break() {
        let rows = [row(&"a".repeat(COLUMNS)), row(""), row("after")];
        let page = compose_history_page(&rows, COLUMNS);
        let term = replay(&page, 6, COLUMNS);
        assert!(!flagged(&term, 0, COLUMNS));
        assert_eq!(row_string(&engine_row(&term, 1)), "");
        assert_eq!(row_string(&engine_row(&term, 2)), "after");
    }

    #[test]
    fn a_blank_row_carrying_only_sgr_is_still_blank() {
        assert_eq!(visible_width(b"\x1b[48;5;4m\x1b[0m"), 0);
        let rows = [row(&"a".repeat(COLUMNS)), row("\x1b[48;5;4m\x1b[0m")];
        let page = compose_history_page(&rows, COLUMNS);
        assert!(page.windows(2).any(|pair| pair == b"\r\n"));
    }

    #[test]
    fn sgr_at_the_wrap_point_does_not_cancel_the_wrap() {
        let filled = format!("\x1b[1m{}\x1b[22m", "b".repeat(COLUMNS));
        let page = compose_history_page(&[row(&filled), row("\x1b[2mtail\x1b[22m")], COLUMNS);
        let term = replay(&page, 6, COLUMNS);
        assert!(flagged(&term, 0, COLUMNS));
    }

    #[test]
    fn combining_marks_measure_as_one_cell() {
        assert_eq!(visible_width("e\u{301}".as_bytes()), 1);
        let nfd = "e\u{301}".repeat(COLUMNS - 1);
        let page = compose_history_page(&[row(&nfd), row("next")], COLUMNS);
        let term = replay(&page, 6, COLUMNS);
        assert!(
            !flagged(&term, 0, COLUMNS),
            "a short row must not be joined"
        );
    }

    #[test]
    fn wide_rows_measure_short_and_stay_hard_broken() {
        // The documented safe direction: a wide row counts one per cluster.
        let wide = "漢".repeat(COLUMNS / 2);
        assert_eq!(visible_width(wide.as_bytes()), COLUMNS / 2);
        let page = compose_history_page(&[row(&wide), row("next")], COLUMNS);
        let term = replay(&page, 6, COLUMNS);
        assert!(!flagged(&term, 0, COLUMNS));
    }

    fn logical_block(columns: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut rows = vec![row(HISTORY_START)];
        let long = "a".repeat(columns * 2 + 5);
        for chunk in long.as_bytes().chunks(columns) {
            rows.push(chunk.to_vec());
        }
        rows.push(row("short row"));
        rows.push(row(HISTORY_END));
        let live =
            format!("{HISTORY_START}\r\n{long}\r\nshort row\r\n{HISTORY_END}\r\n").into_bytes();
        (rows, live)
    }

    fn page_of(rows: &[Vec<u8>]) -> Vec<u8> {
        rows.join(&b"\r\n"[..])
    }

    #[test]
    fn wrap_check_passes_at_the_original_and_changed_widths() {
        let (rows, live) = logical_block(COLUMNS + 30);
        let verdict = wrap_check(&page_of(&rows), &live, COLUMNS + 30);
        assert_eq!(verdict["pass"], true, "{verdict:#}");
        assert_eq!(verdict["continuationsExpected"], 2);
        assert_eq!(verdict["continuationsMarked"], 2);
        assert_eq!(verdict["widthChecks"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn wrap_check_fails_on_a_hard_broken_live_control() {
        // Same rows, but the "live" side really had line breaks: the composed
        // page must not be reported as behaving like it.
        let columns = COLUMNS + 30;
        let (rows, _) = logical_block(columns);
        let live = page_of(&rows);
        let verdict = wrap_check(&page_of(&rows), &live, columns);
        assert_eq!(verdict["pass"], false, "{verdict:#}");
    }

    #[test]
    fn wrap_check_reports_a_page_without_the_block() {
        let verdict = wrap_check(b"nothing here", b"", COLUMNS);
        assert_eq!(verdict["pass"], false);
    }

    #[test]
    fn capture_reads_sgr_without_a_terminal() {
        let capture =
            b"\x1b[1mB\x1b[22mx\x1b[38;5;3my\x1b[39m\x1b[38;2;1;2;3mz\x1b[0m\n\x1b[41m q\x1b[49m\n";
        let rows = parse_capture(capture, 3);
        assert_eq!(row_string(&rows[0]), "Bxyz");
        assert!(rows[0][0].1.bold);
        assert!(!rows[0][1].1.bold);
        assert_eq!(rows[0][2].1.fg, CellColor::Indexed(3));
        assert_eq!(rows[0][3].1.fg, CellColor::Rgb(1, 2, 3));
        assert_eq!(rows[1][0].1.bg, CellColor::Indexed(1));
        assert_eq!(rows[1][1].1.bg, CellColor::Indexed(1));
        assert!(rows[2].is_empty());
    }

    #[test]
    fn a_style_carries_across_captured_rows() {
        // tmux's capture of a bold run that wraps, as it really writes it.
        let rows = parse_capture(b"\x1b[1maa\nbb\x1b[0mcc\n", 2);
        assert!(rows[1][0].1.bold);
        assert!(!rows[1][2].1.bold);
    }

    #[test]
    fn a_class_row_fails_on_attributes_alone() {
        let columns = ANCHOR_COLUMN + 10;
        let painted = format!("\x1b[1mx\x1b[{ANCHOR_COLUMN}G{ANCHOR}");
        let captured = format!("x{}{ANCHOR}", " ".repeat(ANCHOR_COLUMN - 2));
        let term = replay(painted.as_bytes(), 1, columns);
        let truth = parse_capture(captured.as_bytes(), 1);
        let mut parser = vt100::Parser::new(1, columns as u16, 0);
        parser.process(painted.as_bytes());
        let parity = compare_screen(&term, &truth, Some(parser.screen()));
        assert_eq!(parity.text_mismatch_rows, 0);
        assert!(!parity.fixture_rows[0].engine_pass);
    }

    #[test]
    fn engine_and_capture_agree_on_a_plain_styled_row() {
        let term = replay(b"\x1b[1mB\x1b[22mx\x1b[38;5;3my", 3, 20);
        let truth = parse_capture(b"\x1b[1mB\x1b[22mx\x1b[38;5;3my\n", 3);
        let parity = compare_screen(&term, &truth, None);
        assert!(parity.passed(), "{:?}", parity.to_json());
    }

    #[test]
    fn a_named_color_equals_its_index() {
        let term = replay(b"\x1b[31mr\x1b[92mg", 3, 20);
        let truth = parse_capture(b"\x1b[31mr\x1b[92mg\n", 3);
        let parity = compare_screen(&term, &truth, None);
        assert_eq!(parity.attribute_mismatch_cells, 0, "{:?}", parity.to_json());
    }

    /// The whole reason for the anchor: replaying a capture of a row whose
    /// glyph the engine measures wider puts the same text at a different
    /// column, and a text comparison cannot see it.
    #[test]
    fn a_shifted_anchor_fails_even_when_the_text_matches() {
        let columns = ANCHOR_COLUMN + 10;
        // Two cells for tmux, four for the engine: the capture's padding is
        // tmux's, so the engine's anchor lands two columns right of where
        // tmux has it.
        let sample = "\u{1f469}\u{200d}\u{1f4bb}";
        let tag = "[emoji-zwj] ";
        let padding = " ".repeat(ANCHOR_COLUMN - 1 - tag.len() - 2);
        let captured = format!("{tag}{sample}{padding}{ANCHOR}");
        let term = replay(captured.as_bytes(), 1, columns);
        let truth = parse_capture(captured.as_bytes(), 1);
        let mut vt100_parser = vt100::Parser::new(1, columns as u16, 0);
        vt100_parser.process(captured.as_bytes());
        let parity = compare_screen(&term, &truth, Some(vt100_parser.screen()));
        assert_eq!(row_string(&engine_row(&term, 0)), row_string(&truth[0]));
        assert_eq!(parity.text_mismatch_rows, 0);
        assert_eq!(parity.anchor_mismatch_rows, 1, "{:?}", parity.to_json());
        assert!(!parity.passed());
    }

    #[test]
    fn fixture_frames_paint_every_class_where_the_map_says() {
        let columns = 120;
        let term = replay(&fixture_frame(columns, 3), 24, columns);
        for (row, width_row) in WIDTH_ROWS.iter().enumerate() {
            let text = row_string(&engine_row(&term, row));
            assert!(
                text.starts_with(&format!("[{}] ", width_row.class)),
                "{text}"
            );
            assert_eq!(
                engine_anchor_column(&term, row),
                Some(ANCHOR_COLUMN - 1),
                "{text}"
            );
        }
        // The glyph that does not fit moves down whole, with no half left at
        // the margin.
        assert!(row_string(&engine_row(&term, MARGIN_ROW)).starts_with("[margin] xxx"));
        assert!(row_string(&engine_row(&term, MARGIN_ROW + 1)).starts_with("漢 tail"));
        assert!(row_string(&engine_row(&term, STATUS_ROW)).ends_with("WIDTH FRAME 3"));
    }

    #[test]
    fn a_full_width_history_block_composes_to_live_wrapping() {
        let columns = 40;
        let block = history_block(columns);
        // tmux's physical rows for the block, with the SGR run tmux would
        // re-emit at the start of each row.
        let rows = vec![
            row(HISTORY_START),
            row(&"a".repeat(columns)),
            row(&"a".repeat(columns)),
            row("aaaaa"),
            row("short row"),
            row(&format!("\x1b[1m{}", "b".repeat(columns))),
            row(&format!("{}\x1b[0m{}", "b".repeat(10), "c".repeat(15))),
            row(""),
            row(&"d".repeat(columns)),
            row("ddd"),
            row(HISTORY_END),
        ];
        let verdict = wrap_check(&page_of(&rows), &block, columns);
        assert_eq!(verdict["pass"], true, "{verdict:#}");
    }
}

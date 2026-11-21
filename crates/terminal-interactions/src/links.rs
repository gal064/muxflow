use crate::buffer::{Buffer, BufferLine, BufferRange, Position, translate_to_string};
use crate::file_paths::terminal_file_links;
use crate::text::js_regex_static;
use crate::web_links::terminal_web_links;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    File,
    Web,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalDetectedLink {
    pub kind: LinkKind,
    pub text: String,
    /// One-based, inclusive cell range.
    pub range: BufferRange,
}

struct LogicalRow<'a> {
    line: &'a dyn BufferLine,
    row: usize,
    text_start: usize,
    text_end: usize,
    columns: usize,
}

struct LogicalLine<'a> {
    text: String,
    rows: Vec<LogicalRow<'a>>,
    last_row: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    Link(LinkKind),
    /// A rendered Markdown-style `(target)` whose first fragment is not yet a path.
    Delimited,
}

struct HardWrappedLinkCandidate {
    kind: CandidateKind,
    raw_text: String,
    range: BufferRange,
    last_row: usize,
}

/// Bounds work done from a mouse hover over pathological output containing one
/// enormous soft-wrapped line. Ordinary filesystem paths fit comfortably.
const MAX_LOGICAL_ROWS: usize = 128;

/// Bounds application-rendered logical lines and their physical reflow.
const MAX_HARD_LINK_LOGICAL_LINES: usize = 20;
const MAX_HARD_LINK_PHYSICAL_ROWS: usize = MAX_LOGICAL_ROWS;

// A rendered list item marker, at any indentation. Agent TUIs indent nested
// items, so the marker is evidence of a rendered list even when it does not sit
// in the first column.
js_regex_static!(LIST_ITEM_MARKER, r"^\s*(?:[•●*+-]|\d+(?:\.\d+)*[.)])\s");
js_regex_static!(CONTINUATION, r"^ +(\S+)");
js_regex_static!(LIST_ITEM_FRAGMENT, r"\S+[-/]$");
js_regex_static!(ANY_SPACE, r"\s");
js_regex_static!(CLOSING_IN_SUFFIX, r"[\])}]");

/// URL punctuation after which a terminal application may paint the rest on another hard row.
fn is_web_hard_wrap_boundary(character: char) -> bool {
    matches!(
        character,
        '-' | '.'
            | '_'
            | '~'
            | ':'
            | '/'
            | '?'
            | '#'
            | '['
            | '@'
            | '!'
            | '$'
            | '&'
            | '('
            | '*'
            | '+'
            | ','
            | ';'
            | '='
            | '%'
    )
}

/// Finds links in the complete logical line containing one physical buffer row.
///
/// `buffer_line_number` is one-based. A long token can cross a soft-wrap
/// boundary, so parsing a row alone would produce only its prefix or suffix.
/// The terminal's wrap flag is authoritative evidence that no newline occurred;
/// only those rows are joined here.
pub fn terminal_links_for_buffer_line(
    buffer: &dyn Buffer,
    columns: usize,
    buffer_line_number: usize,
) -> Vec<TerminalDetectedLink> {
    if columns == 0 || buffer_line_number < 1 || buffer_line_number > buffer.line_count() {
        return Vec::new();
    }
    let requested_row = buffer_line_number - 1;
    let Some(logical_line) =
        logical_line_containing(buffer, columns, requested_row, MAX_LOGICAL_ROWS)
    else {
        return Vec::new();
    };
    let LogicalLine { rows, text, .. } = &logical_line;

    let detected = terminal_web_links(text)
        .into_iter()
        .map(|link| (LinkKind::Web, link.text, link.start, link.end))
        .chain(
            terminal_file_links(text)
                .into_iter()
                .map(|link| (LinkKind::File, link.text, link.start, link.end)),
        );
    let mut links = Vec::new();
    for (kind, link_text, start, end) in detected {
        let Some(range) = buffer_range(rows, start, end) else {
            continue;
        };
        // A provider is asked about the hovered row. Returning links elsewhere in
        // the same logical line needlessly gives the view ranges it cannot
        // activate at this pointer position.
        if buffer_line_number < range.start.y || buffer_line_number > range.end.y {
            continue;
        }
        links.push(TerminalDetectedLink {
            kind,
            text: link_text,
            range,
        });
    }

    let hard_wrapped = application_hard_wrapped_links(buffer, columns, requested_row);
    if hard_wrapped.is_empty() {
        return links;
    }
    links.retain(|link| {
        !hard_wrapped
            .iter()
            .any(|complete| range_contains(&complete.range, &link.range))
    });
    links.extend(hard_wrapped);
    links.sort_by_key(|link| (link.range.start.y, link.range.start.x));
    links
}

/// Full-screen applications may paint visually wrapped prose as separate hard
/// terminal rows. Reconstruct links only from bounded, kind-specific evidence:
/// known link prefixes, an open target delimiter, and indented continuations.
/// This deliberately does not guess from terminal width because applications
/// can wrap inside an inset content region.
fn application_hard_wrapped_links(
    buffer: &dyn Buffer,
    columns: usize,
    requested_row: usize,
) -> Vec<TerminalDetectedLink> {
    let mut links: Vec<TerminalDetectedLink> = Vec::new();
    for origin in hard_wrapped_candidate_origins(buffer, requested_row) {
        let Some(logical_line) =
            logical_line_starting_at(buffer, columns, origin, MAX_HARD_LINK_PHYSICAL_ROWS)
        else {
            continue;
        };
        let Some(candidate) = hard_wrapped_link_candidate(&logical_line) else {
            continue;
        };
        let Some(extended) =
            extend_application_hard_wrapped_link(buffer, columns, origin, &candidate)
        else {
            continue;
        };
        if range_contains_row(&extended.range, requested_row + 1)
            && !links
                .iter()
                .any(|link| range_contains(&link.range, &extended.range))
        {
            links.push(extended);
        }
    }
    links
}

fn hard_wrapped_candidate_origins(buffer: &dyn Buffer, requested_row: usize) -> Vec<usize> {
    let first_physical_row = requested_row.saturating_sub(MAX_HARD_LINK_PHYSICAL_ROWS - 1);
    let mut origins = Vec::new();
    let mut row = requested_row;
    while row >= first_physical_row && origins.len() < MAX_HARD_LINK_LOGICAL_LINES {
        while row > first_physical_row && buffer.line(row).is_some_and(|line| line.is_wrapped()) {
            row -= 1;
        }
        match buffer.line(row) {
            Some(line) if !line.is_wrapped() => origins.push(row),
            _ => break,
        }
        if row == 0 {
            break;
        }
        row -= 1;
    }
    origins.reverse();
    origins
}

fn hard_wrapped_link_candidate(logical_line: &LogicalLine) -> Option<HardWrappedLinkCandidate> {
    let line_text = logical_line.text.as_str();
    let is_list_item = LIST_ITEM_MARKER.is_match(line_text);
    let opening = unclosed_target_opening(line_text);
    let file = terminal_file_links(line_text)
        .into_iter()
        .find(|candidate| {
            candidate.end == line_text.len()
                && can_continue_hard_wrapped_file_link(&candidate.text)
                && Some(candidate.start) != opening.map(|opening| opening + 1)
        });
    if let Some(file) = file
        && let Some(range) = buffer_range(&logical_line.rows, file.start, file.end)
    {
        return Some(HardWrappedLinkCandidate {
            kind: CandidateKind::Link(LinkKind::File),
            raw_text: file.text,
            range,
            last_row: logical_line.last_row,
        });
    }

    let web = terminal_web_links(line_text)
        .into_iter()
        .rev()
        .find(|candidate| {
            let raw_text = &line_text[candidate.start..];
            let trimmed_suffix = &raw_text[candidate.text.len()..];
            !ANY_SPACE.is_match(raw_text)
                && !CLOSING_IN_SUFFIX.is_match(trimmed_suffix)
                && can_continue_hard_wrapped_web_link(raw_text)
        });
    if let Some(web) = web {
        let range = buffer_range(&logical_line.rows, web.start, line_text.len())?;
        return Some(HardWrappedLinkCandidate {
            kind: CandidateKind::Link(LinkKind::Web),
            raw_text: line_text[web.start..].to_string(),
            range,
            last_row: logical_line.last_row,
        });
    }

    // A rendered Markdown-style target can wrap before its first slash, so its
    // first fragment is not yet a file path. The unmatched opening parenthesis
    // and eventual closing parenthesis provide the missing boundary evidence.
    if let Some(opening) = opening {
        let range = buffer_range(&logical_line.rows, opening + 1, line_text.len())?;
        return Some(HardWrappedLinkCandidate {
            kind: CandidateKind::Delimited,
            raw_text: line_text[opening + 1..].to_string(),
            range,
            last_row: logical_line.last_row,
        });
    }

    // A plain path in a rendered list item can also wrap before its first slash.
    // In that case the first row is not a path yet, but a trailing '-' or '/' is
    // explicit continuation evidence. Accept it as a candidate only; the joined
    // text must still parse as a file path before any link is returned.
    if !is_list_item {
        return None;
    }
    let fragment = LIST_ITEM_FRAGMENT.find(line_text)?;
    let range = buffer_range(&logical_line.rows, fragment.start(), line_text.len())?;
    Some(HardWrappedLinkCandidate {
        kind: CandidateKind::Link(LinkKind::File),
        raw_text: fragment.as_str().to_string(),
        range,
        last_row: logical_line.last_row,
    })
}

/// Reads the complete logical line containing one physical row.
fn logical_line_containing(
    buffer: &dyn Buffer,
    columns: usize,
    requested_row: usize,
    max_rows: usize,
) -> Option<LogicalLine<'_>> {
    let mut first_row = requested_row;
    while first_row > 0 && buffer.line(first_row).is_some_and(|line| line.is_wrapped()) {
        first_row -= 1;
        if requested_row - first_row + 1 > max_rows {
            return None;
        }
    }
    logical_line_starting_at(buffer, columns, first_row, max_rows)
}

/// Reads one logical line beginning at a non-wrapped physical row.
fn logical_line_starting_at(
    buffer: &dyn Buffer,
    columns: usize,
    first_row: usize,
    max_rows: usize,
) -> Option<LogicalLine<'_>> {
    let first_line = buffer.line(first_row)?;
    if first_line.is_wrapped() || max_rows < 1 {
        return None;
    }

    let mut last_row = first_row;
    while last_row + 1 < buffer.line_count()
        && buffer
            .line(last_row + 1)
            .is_some_and(|line| line.is_wrapped())
    {
        last_row += 1;
        if last_row - first_row + 1 > max_rows {
            return None;
        }
    }

    let mut rows = Vec::new();
    let mut text = String::new();
    for row in first_row..=last_row {
        let line = buffer.line(row)?;
        let row_columns = logical_row_columns(buffer, line, row, last_row, columns);
        let row_text = translate_to_string(line, row == last_row, 0, row_columns);
        rows.push(LogicalRow {
            line,
            row,
            text_start: text.len(),
            text_end: text.len() + row_text.len(),
            columns: row_columns,
        });
        text.push_str(&row_text);
    }
    Some(LogicalLine {
        text,
        rows,
        last_row,
    })
}

/// Finds the outermost unmatched parenthesis whose suffix is one token.
fn unclosed_target_opening(text: &str) -> Option<usize> {
    let mut openings = Vec::new();
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => openings.push(index),
            b')' => {
                openings.pop();
            }
            _ => {}
        }
    }
    openings.into_iter().find(|&opening| {
        let suffix = &text[opening + 1..];
        !suffix.is_empty() && !ANY_SPACE.is_match(suffix)
    })
}

/// Finds the closing parenthesis paired with the wrapper before `text`.
fn matching_target_closing(text: &str) -> Option<usize> {
    let mut depth = 1;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn extend_application_hard_wrapped_link(
    buffer: &dyn Buffer,
    columns: usize,
    origin: usize,
    candidate: &HardWrappedLinkCandidate,
) -> Option<TerminalDetectedLink> {
    let mut raw_text = candidate.raw_text.clone();
    let mut extended = None;
    let mut row = candidate.last_row + 1;
    let mut logical_lines = 1;

    while row < buffer.line_count() && logical_lines < MAX_HARD_LINK_LOGICAL_LINES {
        if !can_continue_application_hard_wrapped_link(candidate, &raw_text) {
            break;
        }
        let used_rows = row - origin;
        if used_rows >= MAX_HARD_LINK_PHYSICAL_ROWS {
            return None;
        }
        let logical_line = logical_line_starting_at(
            buffer,
            columns,
            row,
            MAX_HARD_LINK_PHYSICAL_ROWS - used_rows,
        )?;
        logical_lines += 1;
        let line_text = logical_line.text.as_str();
        let Some(continuation) = hard_wrapped_continuation(line_text) else {
            break;
        };
        if candidate.kind == CandidateKind::Link(LinkKind::File)
            && !continues_wrapped_file_path(&raw_text, continuation.text)
        {
            break;
        }

        let combined = format!("{raw_text}{}", continuation.text);
        match candidate.kind {
            CandidateKind::Delimited => {
                let Some(closing) = matching_target_closing(&combined) else {
                    if continuation.end != line_text.len() {
                        break;
                    }
                    raw_text = combined;
                    row = logical_line.last_row + 1;
                    continue;
                };
                let Some((kind, joined_text)) = parse_delimited_target(&combined[..closing]) else {
                    break;
                };
                if joined_text.len() <= raw_text.len() {
                    break;
                }
                let appended = joined_text.len() - raw_text.len();
                let Some(appended_range) = buffer_range(
                    &logical_line.rows,
                    continuation.start,
                    continuation.start + appended,
                ) else {
                    break;
                };
                return Some(TerminalDetectedLink {
                    kind,
                    text: joined_text,
                    range: BufferRange {
                        start: candidate.range.start,
                        end: appended_range.end,
                    },
                });
            }
            CandidateKind::Link(kind) => {
                let joined = match kind {
                    LinkKind::File => terminal_file_links(&combined)
                        .into_iter()
                        .next()
                        .map(|link| (link.start, link.text)),
                    LinkKind::Web => terminal_web_links(&combined)
                        .into_iter()
                        .next()
                        .map(|link| (link.start, link.text)),
                };
                let Some((0, joined_text)) = joined else {
                    break;
                };
                if joined_text.len() <= raw_text.len() {
                    break;
                }
                let appended = joined_text.len() - raw_text.len();
                let Some(appended_range) = buffer_range(
                    &logical_line.rows,
                    continuation.start,
                    continuation.start + appended,
                ) else {
                    break;
                };

                let joined_length = joined_text.len();
                extended = Some(TerminalDetectedLink {
                    kind,
                    text: joined_text,
                    range: BufferRange {
                        start: candidate.range.start,
                        end: appended_range.end,
                    },
                });
                if continuation.end != line_text.len() {
                    break;
                }
                if joined_length != combined.len()
                    && (kind != LinkKind::Web || !can_continue_hard_wrapped_web_link(&combined))
                {
                    break;
                }
                raw_text = combined;
                row = logical_line.last_row + 1;
            }
        }
    }

    if logical_lines >= MAX_HARD_LINK_LOGICAL_LINES
        && row < buffer.line_count()
        && can_continue_application_hard_wrapped_link(candidate, &raw_text)
    {
        return None;
    }
    extended
}

fn can_continue_application_hard_wrapped_link(
    candidate: &HardWrappedLinkCandidate,
    text: &str,
) -> bool {
    match candidate.kind {
        CandidateKind::Delimited => matching_target_closing(text).is_none(),
        CandidateKind::Link(LinkKind::File) => can_continue_hard_wrapped_file_link(text),
        CandidateKind::Link(LinkKind::Web) => can_continue_hard_wrapped_web_link(text),
    }
}

fn can_continue_hard_wrapped_file_link(text: &str) -> bool {
    text.ends_with('-') || text.ends_with('/')
}

fn can_continue_hard_wrapped_web_link(text: &str) -> bool {
    text.chars().last().is_some_and(is_web_hard_wrap_boundary)
}

struct Continuation<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

fn hard_wrapped_continuation(line_text: &str) -> Option<Continuation<'_>> {
    let token = CONTINUATION.captures(line_text)?.get(1)?;
    // A row that opens its own list item is the next item, not the tail of the one
    // above it. Without this, siblings splice into each other: an item ending in
    // '/' or '-' would swallow the next item's marker and link a path that was
    // never on screen.
    if LIST_ITEM_MARKER.is_match(line_text) {
        return None;
    }
    Some(Continuation {
        text: token.as_str(),
        start: token.start(),
        end: token.end(),
    })
}

/// Decides whether the fragment on the next row continues a wrapped path.
///
/// The two boundaries this joiner accepts are not equally strong evidence. A row
/// broken after '-' was split inside a token, because prose does not end a row
/// mid-word, so whatever follows continues that token. A row broken after '/'
/// is also exactly where an ordinary sentence wraps at a space, so there the
/// fragment must still look like part of a path: a separator or an extension.
/// Otherwise it is simply the next word, and joining it invents a path that was
/// never on screen.
fn continues_wrapped_file_path(text: &str, continuation: &str) -> bool {
    text.ends_with('-') || continuation.contains('/') || continuation.contains('.')
}

fn parse_delimited_target(text: &str) -> Option<(LinkKind, String)> {
    if let Some(web) = terminal_web_links(text).into_iter().next()
        && web.start == 0
    {
        return Some((LinkKind::Web, web.text));
    }
    let file = terminal_file_links(text).into_iter().next()?;
    (file.start == 0).then_some((LinkKind::File, file.text))
}

fn range_contains_row(range: &BufferRange, row: usize) -> bool {
    row >= range.start.y && row <= range.end.y
}

fn range_contains(outer: &BufferRange, inner: &BufferRange) -> bool {
    position_key(outer.start) <= position_key(inner.start)
        && position_key(outer.end) >= position_key(inner.end)
}

fn position_key(position: Position) -> (usize, usize) {
    (position.y, position.x)
}

/// A wide glyph cannot start in the final cell. The terminal leaves that cell
/// empty and wraps the glyph onto the next row; the empty cell is layout, not a
/// space in the logical text.
fn logical_row_columns(
    buffer: &dyn Buffer,
    line: &dyn BufferLine,
    row: usize,
    last_row: usize,
    columns: usize,
) -> usize {
    if row == last_row {
        return columns;
    }
    let last_cell = line.cell(columns - 1);
    let next_cell = buffer.line(row + 1).and_then(|next| next.cell(0));
    if last_cell.is_some_and(|cell| cell.chars.is_empty() && cell.width == 1)
        && next_cell.is_some_and(|cell| cell.width == 2)
    {
        return columns - 1;
    }
    columns
}

fn buffer_range(rows: &[LogicalRow], start: usize, end: usize) -> Option<BufferRange> {
    let start_row = rows
        .iter()
        .find(|row| start >= row.text_start && start < row.text_end)?;
    let end_row = rows
        .iter()
        .find(|row| end > row.text_start && end <= row.text_end)?;
    let start_cell = cell_at_string_start(start_row, start - start_row.text_start)?;
    let end_cell = cell_at_string_end(end_row, end - end_row.text_start)?;
    Some(BufferRange {
        start: Position {
            x: start_cell + 1,
            y: start_row.row + 1,
        },
        end: Position {
            x: end_cell + 1,
            y: end_row.row + 1,
        },
    })
}

fn cell_at_string_start(row: &LogicalRow, target: usize) -> Option<usize> {
    let mut offset = 0;
    for cell_index in 0..row.columns {
        let Some(cell) = row.line.cell(cell_index) else {
            continue;
        };
        if cell.width == 0 {
            continue;
        }
        if offset == target {
            return Some(cell_index);
        }
        offset += cell.chars.len().max(1);
        if offset > target {
            return None;
        }
    }
    None
}

fn cell_at_string_end(row: &LogicalRow, target: usize) -> Option<usize> {
    let mut offset = 0;
    for cell_index in 0..row.columns {
        let Some(cell) = row.line.cell(cell_index) else {
            continue;
        };
        if cell.width == 0 {
            continue;
        }
        offset += cell.chars.len().max(1);
        if offset == target {
            return Some(cell_index + cell.width - 1);
        }
        if offset > target {
            return None;
        }
    }
    None
}

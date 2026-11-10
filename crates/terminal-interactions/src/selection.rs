use crate::buffer::{Buffer, BufferRange, translate_to_string};
use crate::text::{is_js_space, js_regex_static, split_js_whitespace, trim_js};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSelectionRow {
    /// Selected text on this physical buffer row, with terminal padding removed.
    pub text: String,
    /// The terminal's own soft-wrap marker. Hard application line breaks leave this false.
    pub is_wrapped: bool,
    /// Cell immediately after the last non-whitespace glyph on the complete buffer row.
    pub occupied_end_cell: usize,
    /// First non-whitespace cell on the complete row, or the terminal width when blank.
    pub first_text_cell: usize,
    /// Display-cell width of the row's first token.
    pub first_token_cells: usize,
    /// Exclusive end of the selected range on this row.
    pub selected_end_cell: usize,
    /// Non-whitespace selected cells and the subset carrying non-default attributes.
    pub text_cells: usize,
    pub styled_text_cells: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSelectionSnapshot {
    /// Exactly what the terminal would otherwise write to the clipboard.
    pub text: String,
    pub columns: usize,
    pub rows: Vec<TerminalSelectionRow>,
    /// False for missing or stale rows and a rectangular selection.
    pub metadata_complete: bool,
}

fn normalized_selection_text(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\u{a0}', " ")
}

fn row_metrics(
    buffer: &dyn Buffer,
    row_index: usize,
    start_cell: usize,
    end_cell: usize,
    terminal_columns: usize,
) -> Option<TerminalSelectionRow> {
    let line = buffer.line(row_index)?;
    let mut occupied_end_cell = 0;
    let mut first_text_cell = terminal_columns;
    let mut first_token_cells = 0;
    let mut inside_first_token = false;
    let mut first_token_finished = false;
    let mut text_cells = 0;
    let mut styled_text_cells = 0;

    for cell_index in 0..terminal_columns {
        let Some(cell) = line.cell(cell_index) else {
            continue;
        };
        if cell.width == 0 {
            continue;
        }
        let whitespace = cell.chars.chars().all(is_js_space);
        let width = cell.width.max(1);
        if !whitespace {
            occupied_end_cell = cell_index + width;
            if first_text_cell == terminal_columns {
                first_text_cell = cell_index;
                inside_first_token = true;
            }
            if inside_first_token && !first_token_finished {
                first_token_cells += width;
            }
            if cell_index >= start_cell && cell_index < end_cell {
                text_cells += 1;
                if !cell.attribute_default {
                    styled_text_cells += 1;
                }
            }
        } else if inside_first_token {
            inside_first_token = false;
            first_token_finished = true;
        }
    }

    Some(TerminalSelectionRow {
        text: translate_to_string(line, true, start_cell, end_cell).replace('\u{a0}', " "),
        is_wrapped: line.is_wrapped(),
        occupied_end_cell,
        first_text_cell,
        first_token_cells,
        selected_end_cell: end_cell,
        text_cells,
        styled_text_cells,
    })
}

/// Takes the text and its physical-buffer evidence in one synchronous read, so
/// later terminal output cannot change the evidence underneath an asynchronous
/// clipboard write.
///
/// `text` is the selected text and `position` its zero-based, end-exclusive
/// range, both as the terminal reports them.
pub fn capture_terminal_selection(
    text: &str,
    position: Option<BufferRange>,
    columns: usize,
    buffer: &dyn Buffer,
) -> TerminalSelectionSnapshot {
    let incomplete = || TerminalSelectionSnapshot {
        text: text.to_string(),
        columns,
        rows: Vec::new(),
        metadata_complete: false,
    };
    let Some(position) = position else {
        return incomplete();
    };
    if text.is_empty() || columns < 1 {
        return incomplete();
    }
    if position.start.y > position.end.y
        || (position.start.y == position.end.y && position.start.x >= position.end.x)
    {
        return incomplete();
    }

    let mut rows = Vec::new();
    for row_index in position.start.y..=position.end.y {
        let start_cell = if row_index == position.start.y {
            position.start.x
        } else {
            0
        };
        let end_cell = if row_index == position.end.y {
            position.end.x
        } else {
            columns
        };
        let Some(row) = row_metrics(buffer, row_index, start_cell, end_cell, columns) else {
            return incomplete();
        };
        rows.push(row);
    }

    // Reproduce the terminal's normal-selection join. A rectangular selection
    // produces different text for the same coordinates and therefore fails closed.
    let mut logical_rows: Vec<String> = Vec::new();
    for row in &rows {
        match logical_rows.last_mut() {
            Some(last) if row.is_wrapped => last.push_str(&row.text),
            _ => logical_rows.push(row.text.clone()),
        }
    }
    let metadata_complete = normalized_selection_text(text) == logical_rows.join("\n");
    TerminalSelectionSnapshot {
        text: text.to_string(),
        columns,
        rows,
        metadata_complete,
    }
}

js_regex_static!(SHELL_OPTION, r"^--?[A-Za-z0-9][A-Za-z0-9-]*$");
js_regex_static!(SHELL_OPTION_ANYWHERE, r"(?:^|\s)--?[A-Za-z0-9]");
js_regex_static!(
    COMMAND_TOKEN,
    r"^(?:[a-z0-9_+.-]+|(?:\.{0,2}|~)?/[a-z0-9_+./-]+)$"
);
js_regex_static!(BARE_COMMAND_TOKEN, r"^[a-z][a-z0-9_+-]*$");
js_regex_static!(PLACEHOLDER_TOKEN, r"^[A-Z][A-Z0-9_-]*$");
js_regex_static!(
    HELP_TEXT,
    r"(?i)(?:^|\s)(?:usage|options|commands):|\[(?:options?|flags?)\]"
);
js_regex_static!(
    CONTROL_KEYWORD,
    r"(?:^|\s)(?:if|then|fi|for|while|until|case|esac|do|done|function|else|elif)(?:\s|$)"
);
js_regex_static!(TRAILING_BACKSLASH, r"\\\s*$");
js_regex_static!(INTERIOR_PADDING, r"\S[ \t]{2,}\S");
js_regex_static!(HYPHEN_SPLIT_TAIL, r"[A-Za-z0-9]-$");
js_regex_static!(HYPHEN_SPLIT_HEAD, r"^[A-Za-z0-9]");

fn has_unsafe_shell_structure(lines: &[&str]) -> bool {
    let mut quote: Option<char> = None;
    for line in lines {
        if TRAILING_BACKSLASH.is_match(line) {
            return true;
        }
        let characters: Vec<char> = line.chars().collect();
        let mut escaped = false;
        for (index, &character) in characters.iter().enumerate() {
            let next = characters.get(index + 1).copied();
            if escaped {
                escaped = false;
                continue;
            }
            if character == '\\' && quote != Some('\'') {
                escaped = true;
                continue;
            }
            if let Some(open) = quote {
                if character == open {
                    quote = None;
                }
                continue;
            }
            if character == '\'' || character == '"' {
                quote = Some(character);
                continue;
            }
            if matches!(
                character,
                '`' | ';' | '|' | '&' | '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>'
            ) || (character == '$' && next == Some('('))
            {
                return true;
            }
        }
        // A quote crossing a physical newline means that newline can be semantic.
        if quote.is_some() || escaped {
            return true;
        }
    }
    false
}

/// A "hyphen" boundary is a word the renderer split after an interior hyphen
/// (textwrap-style): "<word>-" ends the row and the rest of the word starts the
/// next one. Alphanumerics on both sides exclude bare "-" and "--" arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoundaryKind {
    Space,
    Hyphen,
}

fn boundary_kind(previous: &str, next: &str) -> BoundaryKind {
    if HYPHEN_SPLIT_TAIL.is_match(previous) && HYPHEN_SPLIT_HEAD.is_match(next) {
        BoundaryKind::Hyphen
    } else {
        BoundaryKind::Space
    }
}

/// Returns one boundary kind per row break, or `None` when the text must not be joined.
fn shell_shape_allows_join(lines: &[&str]) -> Option<Vec<BoundaryKind>> {
    let trimmed: Vec<&str> = lines.iter().map(|line| trim_js(line)).collect();
    if trimmed
        .iter()
        .any(|line| line.is_empty() || INTERIOR_PADDING.is_match(line))
    {
        return None;
    }
    let first_token = split_js_whitespace(trimmed[0])[0];
    if !COMMAND_TOKEN.is_match(first_token) || first_token.starts_with('-') {
        return None;
    }
    let joined = trimmed.join(" ");
    if HELP_TEXT.is_match(&joined)
        || CONTROL_KEYWORD.is_match(&joined)
        || has_unsafe_shell_structure(&trimmed)
    {
        return None;
    }

    let kinds: Vec<BoundaryKind> = trimmed
        .windows(2)
        .map(|pair| boundary_kind(pair[0], pair[1]))
        .collect();
    // A split word is its own evidence of a display wrap. Space boundaries need
    // the command to look flag-driven before any of them can be trusted.
    if kinds.contains(&BoundaryKind::Space) && SHELL_OPTION_ANYWHERE.find_iter(&joined).count() < 2
    {
        return None;
    }

    for (index, kind) in kinds.iter().enumerate() {
        if *kind == BoundaryKind::Hyphen {
            continue;
        }
        let previous_token = split_js_whitespace(trimmed[index])
            .last()
            .copied()
            .unwrap_or("");
        let next_token = split_js_whitespace(trimmed[index + 1])[0];
        // A new indented command can follow an earlier command that happened to
        // end in a flag. Plain executable-shaped words are too ambiguous to treat
        // as that flag's value; hosts, paths and numeric values remain eligible.
        if BARE_COMMAND_TOKEN.is_match(next_token) || PLACEHOLDER_TOKEN.is_match(next_token) {
            return None;
        }
        if !SHELL_OPTION.is_match(previous_token) && !SHELL_OPTION.is_match(next_token) {
            return None;
        }
    }
    Some(kinds)
}

/// Joins only high-confidence application-rendered command wraps. Text alone
/// can never distinguish a display break from an intentional newline, so every
/// missing or contradictory buffer signal returns the original bytes.
pub fn clean_wrapped_command_selection(snapshot: &TerminalSelectionSnapshot) -> String {
    let unchanged = || snapshot.text.clone();
    let rows = &snapshot.rows;
    if !snapshot.metadata_complete || rows.len() < 2 || rows.len() > 20 {
        return unchanged();
    }
    let normalized = normalized_selection_text(&snapshot.text);
    let lines: Vec<&str> = normalized.split('\n').collect();
    if lines.len() != rows.len() {
        return unchanged();
    }
    let Some(kinds) = shell_shape_allows_join(&lines) else {
        return unchanged();
    };
    if rows
        .iter()
        .any(|row| row.is_wrapped || row.text_cells == 0 || row.styled_text_cells != row.text_cells)
    {
        return unchanged();
    }
    if rows
        .iter()
        .any(|row| row.selected_end_cell < row.occupied_end_cell)
    {
        return unchanged();
    }

    let continuation_gutter = rows[1].first_text_cell;
    if !(1..=8).contains(&continuation_gutter)
        || rows[1..]
            .iter()
            .any(|row| row.first_text_cell != continuation_gutter)
    {
        return unchanged();
    }

    for (index, pair) in rows.windows(2).enumerate() {
        let (row, next) = (&pair[0], &pair[1]);
        if next.first_token_cells < 1 {
            return unchanged();
        }
        // The renderer greedily broke before next's first token: that token, plus
        // one separating space unless it continues a hyphen-split word, did not
        // fit in the cells remaining on this row.
        let separator_cells = if kinds[index] == BoundaryKind::Hyphen {
            0
        } else {
            1
        };
        if row.occupied_end_cell + separator_cells + next.first_token_cells < snapshot.columns {
            return unchanged();
        }
    }

    let mut joined = trim_js(lines[0]).to_string();
    for (index, line) in lines.iter().enumerate().skip(1) {
        if kinds[index - 1] != BoundaryKind::Hyphen {
            joined.push(' ');
        }
        joined.push_str(trim_js(line));
    }
    joined
}

use crate::buffer::BufferLine;
use crate::text::js_regex_static;
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalFileLink {
    pub text: String,
    /// Byte offsets into the line passed to `terminal_file_links`.
    pub start: usize,
    pub end: usize,
}

js_regex_static!(TOKEN, r#"[^\s"'`]+"#);
js_regex_static!(LOCATION_SUFFIX, r":\d+(?::\d+)?$");

// `[` and `]` are escaped inside the classes because Rust reads a bare `[` as a nested class.
static NON_FILE_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?://|[^/(){}]*:|(?:www\.[^/()\[\]{}]*|[a-z0-9-]+(?:\.[a-z0-9-]+)+)/)")
        .expect("valid pattern")
});
static WWW_COMPONENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^www\.").expect("valid pattern"));
static HOST_COMPONENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9-]+(?:\.[a-z0-9-]+)+$").expect("valid pattern"));

fn is_trailing_punctuation(byte: u8) -> bool {
    matches!(byte, b',' | b';' | b':' | b'!' | b'?' | b'.')
}

fn is_opening(byte: u8) -> bool {
    matches!(byte, b'(' | b'[' | b'{')
}

fn opening_for(closing: u8) -> Option<u8> {
    match closing {
        b')' => Some(b'('),
        b']' => Some(b'['),
        b'}' => Some(b'{'),
        _ => None,
    }
}

/// Finds only the deliberately small P0 path vocabulary in one terminal line.
///
/// Whitespace, quotes, and prose delimiters separate tokens. Balanced delimiters
/// inside path components remain part of the filename. Absolute paths and
/// relative paths with a slash qualify; a trailing line/column location is left
/// outside the link. URLs and bare filenames do not qualify.
pub fn terminal_file_links(line: &str) -> Vec<TerminalFileLink> {
    let mut links = Vec::new();
    for (token, start) in file_path_tokens(line) {
        let bytes = token.as_bytes();
        let mut trailing = bytes.len();
        while trailing > 0 && is_trailing_punctuation(bytes[trailing - 1]) {
            trailing -= 1;
        }
        let wrapped = &token[..trailing];
        let text = match LOCATION_SUFFIX.find(wrapped) {
            Some(location) => &wrapped[..location.start()],
            None => wrapped,
        };
        if !is_explicit_terminal_file_path(text) {
            continue;
        }
        links.push(TerminalFileLink {
            text: text.to_string(),
            start,
            end: start + text.len(),
        });
    }
    links
}

/// Splits attached labels and unmatched delimiters without losing valid filenames.
/// Every delimiter is ASCII, so scanning bytes never splits a character.
fn file_path_tokens(line: &str) -> Vec<(&str, usize)> {
    let mut parts = Vec::new();
    for found in TOKEN.find_iter(line) {
        let token = found.as_str();
        let bytes = token.as_bytes();
        let mut first_slash = token.find('/');
        let mut paired: HashMap<usize, usize> = HashMap::new();
        let mut openings: Vec<usize> = Vec::new();
        for (index, &byte) in bytes.iter().enumerate() {
            if is_opening(byte) {
                openings.push(index);
            }
            let Some(opening) = opening_for(byte) else {
                continue;
            };
            match openings.pop() {
                Some(start) if bytes[start] == opening => {
                    paired.insert(start, index);
                }
                _ => openings.clear(),
            }
        }

        let mut start = 0;
        let mut has_slash = false;
        let mut index = 0;
        while index < bytes.len() {
            // A URL or host reference owns the rest of its token. Splitting its
            // unmatched delimiters must not create separate file targets.
            if index == start && NON_FILE_PREFIX.is_match(&token[start..]) {
                start = token.len();
                break;
            }
            let byte = bytes[index];
            if byte == b'/' {
                has_slash = true;
            }
            if !is_opening(byte) && opening_for(byte).is_none() {
                index += 1;
                continue;
            }
            if let Some(&closing) = paired.get(&index)
                && (has_slash || first_slash.is_some_and(|slash| closing < slash))
            {
                index = closing + 1;
                continue;
            }
            if start < index {
                parts.push((&token[start..index], found.start() + start));
            }
            start = index + 1;
            first_slash = token[start..].find('/').map(|offset| start + offset);
            has_slash = false;
            index += 1;
        }
        if start < token.len() {
            parts.push((&token[start..], found.start() + start));
        }
    }
    parts
}

pub fn is_explicit_terminal_file_path(value: &str) -> bool {
    if value.is_empty() || value.contains('\0') || LOCATION_SUFFIX.is_match(value) {
        return false;
    }
    if value.starts_with("//") {
        return false;
    }
    if value.starts_with('/') {
        return true;
    }
    if value.starts_with('~') {
        return value.starts_with("~/") && value.len() > 2 && !value[2..].starts_with('/');
    }
    let Some(separator) = value.find('/') else {
        return false;
    };
    let first_component = &value[..separator];
    !(first_component.contains(':')
        || WWW_COMPONENT.is_match(first_component)
        || HOST_COMPONENT.is_match(first_component))
}

/// Maps a byte range of a line's text onto the line's zero-based, inclusive cell
/// range. The text is the line read as `translate_to_string` does.
pub fn terminal_file_link_cell_range(
    line: &dyn BufferLine,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let mut string_offset = 0;
    let mut start_cell = None;
    for cell_index in 0..line.columns() {
        let Some(cell) = line.cell(cell_index) else {
            continue;
        };
        if cell.width == 0 {
            continue;
        }
        if string_offset == start {
            start_cell = Some(cell_index);
        }
        let next_offset = string_offset + cell.chars.len().max(1);
        if next_offset == end
            && let Some(start_cell) = start_cell
        {
            return Some((start_cell, cell_index + cell.width - 1));
        }
        string_offset = next_offset;
        if string_offset > end {
            return None;
        }
    }
    None
}

/// POSIX lexical resolution of a terminal path against the pane's cwd.
pub fn resolve_terminal_file_path(value: &str, cwd: &str, home: Option<&str>) -> Option<String> {
    if !is_explicit_terminal_file_path(value) {
        return None;
    }
    if let Some(rest) = value.strip_prefix("~/") {
        let home = home.filter(|home| home.starts_with('/') && !home.contains('\0'))?;
        return resolve_posix_path(&format!("{}/{rest}", home.trim_end_matches('/')), "/");
    }
    resolve_posix_path(value, cwd)
}

fn resolve_posix_path(value: &str, cwd: &str) -> Option<String> {
    if value.is_empty() || value.contains('\0') || !cwd.starts_with('/') {
        return None;
    }
    let absolute = if value.starts_with('/') {
        value.to_string()
    } else {
        format!("{}/{value}", cwd.trim_end_matches('/'))
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in absolute.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

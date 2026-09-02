//! Text shaping for voice mode (docs/mobile/voice-mode-plan.md §4.4, §4.5).
//!
//! Two directions. A transcript is typed into an agent's pane by the phone
//! with a trailing CR, so it must be one line: a newline inside it would submit
//! early in Claude's TUI. A reply is markdown written for a screen, so it is
//! flattened into something a voice can read and cut at a sentence when it is
//! too long to listen to.

/// Characters of speech kept from a reply; ~100 s of 48 kbps MP3.
pub(crate) const SPEECH_CAP_CHARS: usize = 1500;
const REST_ON_SCREEN: &str = " The rest of the reply is on screen.";
const CODE_OMITTED: &str = "code omitted.";

/// A transcript as one line: trimmed, every control character (newline,
/// carriage return and tab included) mapped to a space, whitespace runs
/// collapsed.
pub(crate) fn transcript_line(text: &str) -> String {
    let mut line = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !line.is_empty();
            continue;
        }
        if pending_space {
            line.push(' ');
            pending_space = false;
        }
        line.push(character);
    }
    line
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpeechText {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

/// Markdown to speakable prose, capped at `cap` characters on a sentence
/// boundary.
///
/// Fenced blocks become "code omitted."; inline code keeps its content;
/// headings become sentences; list markers, blockquotes, emphasis, rules, HTML
/// tags and table separators are dropped; links keep their text, images their
/// alt text, bare URLs become "link"; table cells are separated by commas.
/// Block boundaries that lack terminal punctuation get a period so sentences
/// do not run together when spoken.
pub(crate) fn speech_text(markdown: &str, cap: usize) -> SpeechText {
    let mut spoken = String::new();
    let mut fence: Option<(char, usize)> = None;
    let mut after_blank = true;
    for raw in markdown.lines() {
        let trimmed = raw.trim();
        if let Some((marker, count)) = fence_marker(trimmed) {
            match fence {
                Some((open, width)) if open == marker && count >= width => fence = None,
                Some(_) => {}
                None => {
                    fence = Some((marker, count));
                    append_segment(&mut spoken, CODE_OMITTED, true);
                }
            }
            after_blank = true;
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if trimmed.is_empty() || is_rule(trimmed) {
            after_blank = true;
            continue;
        }
        if is_table_separator(trimmed) {
            continue;
        }
        let mut starts_block = after_blank;
        after_blank = false;
        let mut body = trimmed;
        while let Some(rest) = body.strip_prefix('>') {
            body = rest.trim_start();
            starts_block = true;
        }
        let heading = strip_heading(body);
        if let Some(rest) = heading {
            body = rest;
            starts_block = true;
        }
        if let Some(rest) = strip_list_marker(body) {
            body = rest;
            starts_block = true;
        }
        let is_table_row = body.starts_with('|') || body.contains(" | ");
        let row = if is_table_row {
            starts_block = true;
            table_row(body)
        } else {
            body.to_owned()
        };
        let mut text = collapse_whitespace(&strip_inline(&row));
        if text.is_empty() {
            continue;
        }
        if heading.is_some() && !ends_terminal(&text) {
            text.push('.');
        }
        append_segment(&mut spoken, &text, starts_block);
    }
    cap_speech(spoken, cap)
}

fn append_segment(spoken: &mut String, segment: &str, starts_block: bool) {
    if spoken.is_empty() {
        spoken.push_str(segment);
        return;
    }
    if starts_block && !ends_terminal(spoken) {
        spoken.push('.');
    }
    spoken.push(' ');
    spoken.push_str(segment);
}

fn cap_speech(text: String, cap: usize) -> SpeechText {
    if text.chars().count() <= cap {
        return SpeechText {
            text,
            truncated: false,
        };
    }
    let prefix: String = text.chars().take(cap).collect();
    let indices: Vec<(usize, char)> = prefix.char_indices().collect();
    let sentence_end = indices
        .iter()
        .enumerate()
        .filter(|(position, (_, character))| {
            matches!(character, '.' | '!' | '?')
                && indices
                    .get(position + 1)
                    .is_some_and(|(_, next)| next.is_whitespace())
        })
        .map(|(_, (index, character))| index + character.len_utf8())
        .next_back();
    let cut =
        sentence_end.unwrap_or_else(|| prefix.rfind(char::is_whitespace).unwrap_or(prefix.len()));
    let mut spoken = prefix[..cut].trim_end().to_owned();
    spoken.push_str(REST_ON_SCREEN);
    SpeechText {
        text: spoken,
        truncated: true,
    }
}

fn ends_terminal(text: &str) -> bool {
    text.trim_end()
        .chars()
        .next_back()
        .is_some_and(|character| matches!(character, '.' | '!' | '?' | ':' | ';'))
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let marker = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let count = line.chars().take_while(|c| *c == marker).count();
    (count >= 3).then_some((marker, count))
}

fn is_rule(line: &str) -> bool {
    let mut marker = None;
    let mut count = 0;
    for character in line.chars() {
        if character.is_whitespace() {
            continue;
        }
        if !matches!(character, '-' | '*' | '_') || marker.is_some_and(|m| m != character) {
            return false;
        }
        marker = Some(character);
        count += 1;
    }
    count >= 3
}

fn is_table_separator(line: &str) -> bool {
    line.contains('-')
        && line.contains('|')
        && line
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

fn strip_heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(char::is_whitespace) && !rest.is_empty() {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim())
}

fn strip_list_marker(line: &str) -> Option<&str> {
    if let Some(rest) = line
        .strip_prefix('-')
        .or_else(|| line.strip_prefix('*'))
        .or_else(|| line.strip_prefix('+'))
        && rest.starts_with(char::is_whitespace)
    {
        return Some(strip_task_box(rest.trim_start()));
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits <= 9 {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))
            && rest.starts_with(char::is_whitespace)
        {
            return Some(strip_task_box(rest.trim_start()));
        }
    }
    None
}

fn strip_task_box(item: &str) -> &str {
    ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find_map(|marker| item.strip_prefix(marker))
        .unwrap_or(item)
}

fn table_row(line: &str) -> String {
    line.trim_matches('|')
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Inline markdown to plain text, one pass over the characters.
fn strip_inline(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        // `![alt](url)` and `[text](url)`: keep the bracketed text only.
        let link_start = if character == '[' {
            Some(index)
        } else if character == '!' && characters.get(index + 1) == Some(&'[') {
            Some(index + 1)
        } else {
            None
        };
        if let Some(open) = link_start
            && let Some((inner, end)) = bracket_link(&characters, open)
        {
            out.push_str(&strip_inline(&inner));
            index = end;
            continue;
        }
        // Bare URLs read as "link".
        if character == 'h'
            && (starts_with(&characters, index, "http://")
                || starts_with(&characters, index, "https://"))
        {
            let mut end = index;
            while end < characters.len()
                && !characters[end].is_whitespace()
                && !matches!(characters[end], ')' | '>' | '"' | '\'')
            {
                end += 1;
            }
            out.push_str("link");
            index = end;
            continue;
        }
        // HTML tags are dropped; a lone `<` that never closes is kept.
        if character == '<'
            && characters
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, '/' | '!'))
            && let Some(close) = characters[index..].iter().position(|c| *c == '>')
        {
            index += close + 1;
            continue;
        }
        match character {
            '`' | '*' => {}
            '~' if characters.get(index + 1) == Some(&'~') => index += 1,
            '_' => {
                let bound = |position: Option<&char>| position.is_none_or(|c| !c.is_alphanumeric());
                let previous = index.checked_sub(1).and_then(|p| characters.get(p));
                if !(bound(previous) || bound(characters.get(index + 1))) {
                    out.push('_');
                }
            }
            _ => out.push(character),
        }
        index += 1;
    }
    out
}

fn starts_with(characters: &[char], index: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, expected)| characters.get(index + offset) == Some(&expected))
}

/// `[text](url)` starting at `open`: the text and the index after `)`.
fn bracket_link(characters: &[char], open: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (offset, character) in characters[open..].iter().enumerate() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if characters.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = characters[close + 2..].iter().position(|c| *c == ')')? + close + 3;
    Some((characters[open + 1..close].iter().collect(), end))
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending = !out.is_empty();
            continue;
        }
        if pending {
            out.push(' ');
            pending = false;
        }
        out.push(character);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_line_maps_every_control_character_to_one_space_and_trims() {
        assert_eq!(
            transcript_line("  list the\nfiles\r\n\tin this   directory \u{7}\n"),
            "list the files in this directory"
        );
        assert_eq!(transcript_line("\n\n"), "");
        assert_eq!(transcript_line("héllo\u{200b}wörld"), "héllo\u{200b}wörld");
    }

    #[test]
    fn markdown_becomes_speakable_prose() {
        let cases: &[(&str, &str)] = &[
            ("# Done\nAll tests pass", "Done. All tests pass"),
            ("## Summary\n\nFixed the bug.", "Summary. Fixed the bug."),
            (
                "Here is code:\n```rust\nfn main() {}\n```\nAnd after",
                "Here is code: code omitted. And after",
            ),
            ("~~~\nx\n~~~\n", "code omitted."),
            ("Run `cargo test` now", "Run cargo test now"),
            (
                "- first\n- second item\n1. third\n2) fourth",
                "first. second item. third. fourth",
            ),
            ("- [ ] todo\n- [x] done", "todo. done"),
            ("> quoted words\n> more", "quoted words more"),
            (
                "**bold** and *italic* and __under__ and _em_ but snake_case",
                "bold and italic and under and em but snake_case",
            ),
            (
                "See [the docs](https://example.com/x) and ![diagram](img.png)",
                "See the docs and diagram",
            ),
            (
                "Visit https://example.com/path?x=1 today",
                "Visit link today",
            ),
            ("| a | b |\n| --- | --- |\n| 1 | 2 |", "a, b. 1, 2"),
            ("<b>tag</b> stays <br/> text", "tag stays text"),
            ("one\n\n---\n\ntwo", "one. two"),
            ("first line\nsame paragraph", "first line same paragraph"),
            ("Ends with question?\n\nNext", "Ends with question? Next"),
            ("~~struck~~ text", "struck text"),
            ("", ""),
            ("```\nonly code\n```", "code omitted."),
        ];
        for (markdown, expected) in cases {
            let spoken = speech_text(markdown, SPEECH_CAP_CHARS);
            assert_eq!(spoken.text, *expected, "{markdown:?}");
            assert!(!spoken.truncated, "{markdown:?}");
        }
    }

    #[test]
    fn long_replies_are_cut_at_a_sentence_and_flagged() {
        let sentence = "This is a sentence of some length. ";
        let long = sentence.repeat(20);
        let spoken = speech_text(&long, 100);
        assert!(spoken.truncated);
        assert!(spoken.text.ends_with(REST_ON_SCREEN), "{}", spoken.text);
        let body = spoken.text.strip_suffix(REST_ON_SCREEN).unwrap();
        assert!(body.ends_with("length."), "{body}");
        assert!(body.chars().count() <= 100);

        let no_sentence = "word ".repeat(50);
        let spoken = speech_text(&no_sentence, 23);
        assert!(spoken.truncated);
        assert_eq!(spoken.text, format!("word word word word{REST_ON_SCREEN}"));

        let one_word = "x".repeat(40);
        let spoken = speech_text(&one_word, 10);
        assert!(spoken.truncated);
        assert_eq!(spoken.text, REST_ON_SCREEN.trim_start());

        let exact = "abc.";
        assert_eq!(
            speech_text(exact, 4),
            SpeechText {
                text: "abc.".into(),
                truncated: false
            }
        );
    }

    #[test]
    fn a_multibyte_reply_is_cut_on_character_boundaries() {
        let text = "日本語の文章です。".repeat(30);
        let spoken = speech_text(&text, 40);
        assert!(spoken.truncated);
        assert!(spoken.text.chars().count() <= 40 + REST_ON_SCREEN.chars().count());
    }
}

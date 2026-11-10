use crate::text::js_regex_static;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalWebLink {
    pub text: String,
    /// Byte offsets into the line passed to `terminal_web_links`.
    pub start: usize,
    pub end: usize,
}

js_regex_static!(WEB_URL, r#"https?://[^\s<>"']+"#);

fn is_sentence_punctuation(byte: u8) -> bool {
    matches!(byte, b'.' | b',' | b';' | b':' | b'!' | b'?' | b'`')
}

fn opening_delimiter(closing: u8) -> Option<u8> {
    match closing {
        b')' => Some(b'('),
        b']' => Some(b'['),
        b'}' => Some(b'{'),
        _ => None,
    }
}

/// Finds HTTP(S) URLs without swallowing the prose punctuation after them.
pub fn terminal_web_links(line: &str) -> Vec<TerminalWebLink> {
    WEB_URL
        .find_iter(line)
        .map(|candidate| {
            let length = terminal_url_length(candidate.as_str());
            TerminalWebLink {
                text: candidate.as_str()[..length].to_string(),
                start: candidate.start(),
                end: candidate.start() + length,
            }
        })
        .collect()
}

fn terminal_url_length(candidate: &str) -> usize {
    let bytes = candidate.as_bytes();
    let count =
        |delimiter: u8, end: usize| bytes[..end].iter().filter(|&&b| b == delimiter).count();
    let mut end = bytes.len();
    while end > 0 {
        let trailing = bytes[end - 1];
        if is_sentence_punctuation(trailing) {
            end -= 1;
            continue;
        }
        let Some(opening) = opening_delimiter(trailing) else {
            break;
        };
        if count(opening, end) >= count(trailing, end) {
            break;
        }
        end -= 1;
    }
    end
}

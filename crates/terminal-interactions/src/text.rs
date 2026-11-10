//! JavaScript-compatible text helpers.
//!
//! The mobile package runs these rules through JavaScript's `\s`, `\d` and
//! `trim`, and the shared fixtures require identical results. Rust's defaults
//! differ: `\s` includes U+0085 but not U+FEFF, and `\d` matches every Unicode
//! digit.

use regex::Regex;

const JS_SPACE_CLASS: &str = r"[\s\x{FEFF}--\x{85}]";

/// True for the characters JavaScript treats as `\s`.
pub(crate) fn is_js_space(character: char) -> bool {
    character == '\u{FEFF}' || (character.is_whitespace() && character != '\u{85}')
}

pub(crate) fn trim_js(value: &str) -> &str {
    value.trim_matches(is_js_space)
}

/// Splits like JavaScript's `value.split(/\s+/u)` on an already trimmed value.
pub(crate) fn split_js_whitespace(value: &str) -> Vec<&str> {
    value
        .split(is_js_space)
        .filter(|part| !part.is_empty())
        .collect()
}

/// Compiles a pattern written with JavaScript's `\s`, `\S` and `\d`.
pub(crate) fn js_regex(pattern: &str) -> Regex {
    let mut translated = String::with_capacity(pattern.len() + 16);
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            translated.push(character);
            continue;
        }
        match characters.next() {
            Some('s') => translated.push_str(JS_SPACE_CLASS),
            Some('S') => {
                translated.push_str("[^");
                translated.push_str(JS_SPACE_CLASS);
                translated.push(']');
            }
            Some('d') => translated.push_str("[0-9]"),
            Some(escaped) => {
                translated.push('\\');
                translated.push(escaped);
            }
            None => translated.push('\\'),
        }
    }
    Regex::new(&translated).expect("terminal interaction patterns are valid")
}

/// Defines a lazily compiled `js_regex` static.
macro_rules! js_regex_static {
    ($name:ident, $pattern:expr) => {
        static $name: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| crate::text::js_regex($pattern));
    };
}
pub(crate) use js_regex_static;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_follows_javascript() {
        assert!(is_js_space('\u{FEFF}') && is_js_space('\u{a0}') && is_js_space('\u{3000}'));
        assert!(!is_js_space('\u{85}'));
        assert_eq!(trim_js("\u{FEFF} a \u{85}"), "a \u{85}");
    }

    #[test]
    fn translated_classes_follow_javascript() {
        let pattern = js_regex(r"^\d+\s\S+$");
        assert!(pattern.is_match("12 ab"));
        assert!(!pattern.is_match("\u{663} ab"));
        assert!(pattern.is_match("1\u{FEFF}ab"));
        assert!(!pattern.is_match("1\u{85}ab"));
        assert!(js_regex(r#"[^\s"]+"#).is_match("a"));
    }
}

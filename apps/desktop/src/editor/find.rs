//! Find, replace and go to line: what Monaco's find widget and its Go to Line
//! box did. The matching and the labels live here; the bar that drives them is
//! drawn by the document pane. Port of `features/files/searchCount.ts`, whose
//! count the bar shows.
//!
//! Matching is Monaco's three toggles over the whole buffer: match case, whole
//! word and regular expression. Counting stops past a limit so a one-letter
//! query in a long file does not collect every match it has.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

/// Where counting and highlighting stop.
pub const SEARCH_COUNT_LIMIT: usize = 1000;

/// What the find field asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
}

impl Query {
    /// The matcher, `None` without a query, and an error for a pattern that
    /// does not compile, which the bar shows instead of a count.
    pub fn compile(&self) -> Option<Result<Regex, String>> {
        if self.text.is_empty() {
            return None;
        }
        let pattern = if self.regex {
            self.text.clone()
        } else {
            regex::escape(&self.text)
        };
        let pattern = if self.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        };
        Some(
            RegexBuilder::new(&pattern)
                .case_insensitive(!self.match_case)
                .multi_line(true)
                // `$` before a `\r\n` and `.` short of its `\r`, as Monaco
                // reads lines.
                .crlf(true)
                .build()
                .map_err(|error| error.to_string()),
        )
    }
}

/// The query's matches, up to one past the limit so going over it shows. An
/// empty match finds nothing to select, so it is skipped.
pub fn matches(matcher: &Regex, text: &str) -> Vec<Range<usize>> {
    matcher
        .find_iter(text)
        .filter(|found| !found.is_empty())
        .map(|found| found.range())
        .take(SEARCH_COUNT_LIMIT + 1)
        .collect()
}

/// "2 of 5": the match the selection is on, out of how many. "?" when the
/// selection is on none of them, as after typing a query before stepping to a
/// match.
pub fn count_label(matches: &[Range<usize>], selection: &Range<usize>) -> String {
    if matches.is_empty() {
        return "No results".into();
    }
    let current = matches
        .iter()
        .position(|found| found == selection)
        .map(|index| index + 1)
        .filter(|index| *index <= SEARCH_COUNT_LIMIT);
    let of = if matches.len() > SEARCH_COUNT_LIMIT {
        format!("{SEARCH_COUNT_LIMIT}+")
    } else {
        matches.len().to_string()
    };
    match current {
        Some(current) => format!("{current} of {of}"),
        None => format!("? of {of}"),
    }
}

/// The first match starting at or after `from`, wrapping to the top. The
/// search starts inside the whole text, so `\b` and `^` still see what comes
/// before `from`.
pub fn next(matcher: &Regex, text: &str, from: usize) -> Option<Range<usize>> {
    let after = |start: usize| {
        let mut at = start;
        while at <= text.len() {
            let found = matcher.find_at(text, at)?;
            if !found.is_empty() {
                return Some(found.range());
            }
            at = found.end() + text[found.end()..].chars().next().map_or(1, char::len_utf8);
        }
        None
    };
    let from = (0..=from.min(text.len()))
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    after(from).or_else(|| after(0))
}

/// The last match ending before `before`, wrapping to the bottom.
pub fn previous(matcher: &Regex, text: &str, before: usize) -> Option<Range<usize>> {
    let mut last_before = None;
    let mut last = None;
    for found in matcher.find_iter(text).filter(|found| !found.is_empty()) {
        if found.start() < before {
            last_before = Some(found.range());
        }
        last = Some(found.range());
    }
    last_before.or(last)
}

/// A replacement in Monaco's JavaScript syntax, `$1`, `$&` and `$$`, in the
/// regex crate's, which would read `$1_x` as a group named `1_x`.
fn template(with: &str) -> String {
    let mut out = String::with_capacity(with.len());
    let mut rest = with;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out.push_str(&format!("${{{}}}", &after[..digits]));
            rest = &after[digits..];
        } else if let Some(tail) = after.strip_prefix('&') {
            out.push_str("${0}");
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('$') {
            out.push_str("$$");
            rest = tail;
        } else {
            // A lone `$` is literal in JavaScript.
            out.push_str("$$");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// What one match becomes: `$1` and `$&` expand in a regular expression, and
/// a plain query's replacement is taken literally.
pub fn replacement(
    matcher: &Regex,
    query: &Query,
    text: &str,
    found: &Range<usize>,
    with: &str,
) -> String {
    if !query.regex {
        return with.to_owned();
    }
    let mut expanded = String::new();
    if let Some(captures) = matcher.captures_at(text, found.start) {
        captures.expand(&template(with), &mut expanded);
    }
    expanded
}

/// The whole buffer with every match the bar counts replaced: an empty
/// match is not one, here as when counting and stepping.
pub fn replace_all(matcher: &Regex, query: &Query, text: &str, with: &str) -> (String, usize) {
    let template = query.regex.then(|| template(with));
    let mut out = String::with_capacity(text.len());
    let (mut last, mut count) = (0, 0);
    for captures in matcher.captures_iter(text) {
        let found = captures.get(0).expect("the whole match");
        if found.is_empty() {
            continue;
        }
        out.push_str(&text[last..found.start()]);
        match &template {
            Some(template) => captures.expand(template, &mut out),
            None => out.push_str(with),
        }
        last = found.end();
        count += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// A line, and optionally a column, both 1-based: `12`, `12:4` or `12,4`.
pub fn parse_line(input: &str) -> Option<(usize, Option<usize>)> {
    let input = input.trim();
    let (line, column) = match input.split_once([':', ',']) {
        Some((line, column)) => (line, Some(column.trim())),
        None => (input, None),
    };
    let line = line.trim().parse::<usize>().ok().filter(|line| *line > 0)?;
    let column = match column {
        Some(column) => Some(column.parse::<usize>().ok().filter(|column| *column > 0)?),
        None => None,
    };
    Some((line, column))
}

/// Monaco's Go to Line copy for what has been typed so far.
pub fn line_hint(input: &str, current: (usize, usize), lines: usize) -> String {
    match parse_line(input) {
        Some((line, Some(column))) if line <= lines => {
            format!("Go to line {line} and character {column}.")
        }
        Some((line, None)) if line <= lines => format!("Go to line {line}."),
        _ => format!(
            "Current Line: {}, Character: {}. Type a line number between 1 and {lines} to navigate to.",
            current.0, current.1
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> Query {
        Query {
            text: text.into(),
            ..Query::default()
        }
    }

    fn found(query: &Query, text: &str) -> Vec<Range<usize>> {
        matches(&query.compile().unwrap().unwrap(), text)
    }

    #[test]
    fn says_which_match_the_selection_is_on_out_of_how_many() {
        let text = "a cat, a cat, a cat";
        assert_eq!(count_label(&found(&query("cat"), text), &(9..12)), "2 of 3");
    }

    #[test]
    fn does_not_know_which_match_when_the_selection_is_on_none() {
        assert_eq!(
            count_label(&found(&query("cat"), "a cat, a cat"), &(0..0)),
            "? of 2"
        );
    }

    #[test]
    fn says_when_there_is_nothing_to_find_and_nothing_without_a_query() {
        assert_eq!(
            count_label(&found(&query("dog"), "a cat"), &(0..0)),
            "No results"
        );
        assert!(query("").compile().is_none());
    }

    #[test]
    fn stops_counting_at_the_limit() {
        let text = "x".repeat(SEARCH_COUNT_LIMIT + 5);
        let all = found(&query("x"), &text);
        assert_eq!(all.len(), SEARCH_COUNT_LIMIT + 1);
        assert_eq!(
            count_label(&all, &(0..1)),
            format!("1 of {SEARCH_COUNT_LIMIT}+")
        );
        let last = text.len() - 1..text.len();
        assert_eq!(
            count_label(&all, &last),
            format!("? of {SEARCH_COUNT_LIMIT}+")
        );
    }

    #[test]
    fn the_count_follows_the_selection_and_the_edits() {
        let text = "one two one two";
        let matcher = query("two").compile().unwrap().unwrap();
        let first = next(&matcher, text, 0).unwrap();
        assert_eq!(count_label(&matches(&matcher, text), &first), "1 of 2");
        let second = next(&matcher, text, first.end).unwrap();
        assert_eq!(count_label(&matches(&matcher, text), &second), "2 of 2");
        let edited = format!("two {text}");
        let moved = second.start + 4..second.end + 4;
        assert_eq!(count_label(&matches(&matcher, &edited), &moved), "3 of 3");
    }

    #[test]
    fn the_toggles_are_case_whole_word_and_regex() {
        let text = "Cat cat concat CAT";
        assert_eq!(found(&query("cat"), text).len(), 4);
        let case = Query {
            match_case: true,
            ..query("cat")
        };
        assert_eq!(found(&case, text), [4..7, 11..14]);
        let word = Query {
            whole_word: true,
            ..query("cat")
        };
        assert_eq!(found(&word, text), [0..3, 4..7, 15..18]);
        let pattern = Query {
            regex: true,
            ..query("c.t$")
        };
        assert_eq!(found(&pattern, "cat\ncot\nxx"), [0..3, 4..7]);
        // A plain query is literal, regex characters and all.
        let literal = found(&query("a.b"), "a.b axb");
        assert_eq!((literal.len(), literal[0].clone()), (1, 0..3));
        let broken = Query {
            regex: true,
            ..query("(")
        };
        assert!(broken.compile().unwrap().is_err());
    }

    #[test]
    fn stepping_wraps_around_the_buffer() {
        let text = "ab ab ab";
        let matcher = query("ab").compile().unwrap().unwrap();
        assert_eq!(next(&matcher, text, 1), Some(3..5));
        assert_eq!(next(&matcher, text, 7), Some(0..2));
        assert_eq!(previous(&matcher, text, 3), Some(0..2));
        assert_eq!(previous(&matcher, text, 0), Some(6..8));
        assert_eq!(
            next(&query("zz").compile().unwrap().unwrap(), text, 0),
            None
        );
    }

    #[test]
    fn replaces_literally_or_with_captures() {
        let plain = query("a$1");
        let matcher = plain.compile().unwrap().unwrap();
        assert_eq!(
            replace_all(&matcher, &plain, "a$1 a$1", "$0"),
            ("$0 $0".into(), 2)
        );
        let groups = Query {
            regex: true,
            ..query(r"(\w+)=(\w+)")
        };
        let matcher = groups.compile().unwrap().unwrap();
        let text = "a=1 b=2";
        assert_eq!(
            replacement(&matcher, &groups, text, &(4..7), "$2=$1"),
            "2=b"
        );
        assert_eq!(replace_all(&matcher, &groups, text, "$2=$1").0, "1=a 2=b");
        // JavaScript's reading: group 1, then `_x`; the whole match; a dollar.
        assert_eq!(replace_all(&matcher, &groups, "a=1", "$1_x").0, "a_x");
        assert_eq!(
            replace_all(&matcher, &groups, "a=1", "[$&] $$ $").0,
            "[a=1] $ $"
        );
    }

    #[test]
    fn lines_end_before_a_carriage_return_and_empty_matches_are_left_alone() {
        let pattern = Query {
            regex: true,
            ..query("o.*$")
        };
        let matcher = pattern.compile().unwrap().unwrap();
        assert_eq!(
            replace_all(&matcher, &pattern, "foo\r\nbar", "X"),
            ("fX\r\nbar".into(), 1)
        );
        let stars = Query {
            regex: true,
            ..query("x*")
        };
        let matcher = stars.compile().unwrap().unwrap();
        assert_eq!(replace_all(&matcher, &stars, "axb", "Y"), ("aYb".into(), 1));
    }

    #[test]
    fn stepping_from_inside_a_word_keeps_the_word_boundary() {
        let word = Query {
            whole_word: true,
            ..query("cat")
        };
        let matcher = word.compile().unwrap().unwrap();
        let text = "concat cat";
        assert_eq!(next(&matcher, text, 3), Some(7..10));
    }

    #[test]
    fn reads_a_line_and_an_optional_column() {
        assert_eq!(parse_line("12"), Some((12, None)));
        assert_eq!(parse_line(" 12:4 "), Some((12, Some(4))));
        assert_eq!(parse_line("12,4"), Some((12, Some(4))));
        assert_eq!(parse_line("0"), None);
        assert_eq!(parse_line("x"), None);
        assert_eq!(parse_line("3:"), None);
        assert_eq!(line_hint("7", (1, 1), 20), "Go to line 7.");
        assert_eq!(
            line_hint("7:2", (1, 1), 20),
            "Go to line 7 and character 2."
        );
        assert_eq!(
            line_hint("", (3, 5), 20),
            "Current Line: 3, Character: 5. Type a line number between 1 and 20 to navigate to."
        );
        assert_eq!(
            line_hint("99", (3, 5), 20),
            "Current Line: 3, Character: 5. Type a line number between 1 and 20 to navigate to."
        );
    }
}

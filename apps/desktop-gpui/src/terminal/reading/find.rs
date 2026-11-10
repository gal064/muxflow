//! Find in terminal: a literal, case-insensitive search of the whole buffer,
//! as xterm's search addon runs it, with alacritty's regex search underneath.

use std::cell::RefCell;
use std::ops::RangeInclusive;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use gpui_kit::{Entity, Subscription, component::input::InputState};

pub type Match = RangeInclusive<Point>;

pub struct Find {
    pub input: Entity<InputState>,
    /// The query the last search ran, which the highlights keep showing until
    /// the next one, as xterm's decorations do.
    pub regex: RefCell<Option<RegexSearch>>,
    pub active: Option<Match>,
    pub miss: bool,
    pub _subscription: Subscription,
}

pub fn compile(query: &str) -> Option<RegexSearch> {
    // alacritty keeps a combining mark on its base cell, where the search does
    // not see it, so a query with one would never match.
    let marks = regex::Regex::new(r"\p{M}").expect("a valid pattern");
    let query = marks.replace_all(query, "");
    if query.is_empty() {
        return None;
    }
    // `(?i)` overrides alacritty's smart case: xterm's search ignores case.
    RegexSearch::new(&format!("(?i){}", regex::escape(&query))).ok()
}

/// The next match after `active`, wrapping around the buffer. With nothing
/// found yet, "next" starts at the oldest row and "previous" at the newest.
pub fn search<T>(
    term: &Term<T>,
    regex: &mut RegexSearch,
    active: Option<&Match>,
    forward: bool,
) -> Option<Match> {
    let (origin, direction, side) = match (active, forward) {
        (Some(found), true) => (
            found.end().add(term, Boundary::None, 1),
            Direction::Right,
            Side::Left,
        ),
        (Some(found), false) => (
            found.start().sub(term, Boundary::None, 1),
            Direction::Left,
            Side::Right,
        ),
        (None, true) => (
            Point::new(term.topmost_line(), Column(0)),
            Direction::Right,
            Side::Left,
        ),
        (None, false) => (
            Point::new(term.bottommost_line(), term.last_column()),
            Direction::Left,
            Side::Right,
        ),
    };
    term.search_next(regex, origin, direction, side, None)
}

/// Every match on screen.
pub fn visible<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<Match> {
    let top = -(term.grid().display_offset() as i32);
    let start = Point::new(Line(top), Column(0));
    let end = Point::new(
        Line(top + term.screen_lines() as i32 - 1),
        term.last_column(),
    );
    RegexIter::new(start, end, Direction::Right, term, regex).collect()
}

#[cfg(test)]
mod tests {
    use super::super::buffer::tests::term;
    use super::super::selection::point;
    use super::*;

    #[test]
    fn walks_matches_from_the_oldest_row_and_wraps_around() {
        let term = term(20, 3, b"Foo one\r\nbar\r\nfoo two\r\nbaz\r\nFOO three");
        let mut regex = compile("foo").unwrap();
        let top = term.topmost_line().0;
        let first = search(&term, &mut regex, None, true).unwrap();
        assert_eq!(first, point(top, 0)..=point(top, 2), "case ignored");
        let second = search(&term, &mut regex, Some(&first), true).unwrap();
        assert_eq!(second.start().line.0, top + 2);
        let third = search(&term, &mut regex, Some(&second), true).unwrap();
        assert_eq!(third.start().line.0, 2);
        let wrapped = search(&term, &mut regex, Some(&third), true).unwrap();
        assert_eq!(wrapped, first);
        let back = search(&term, &mut regex, Some(&first), false).unwrap();
        assert_eq!(back, third);
        let last = search(&term, &mut regex, None, false).unwrap();
        assert_eq!(last, third);
    }

    #[test]
    fn a_query_is_literal_text() {
        let term = term(20, 2, b"a.b axb (x)");
        let mut regex = compile("a.b").unwrap();
        let found = search(&term, &mut regex, None, true).unwrap();
        assert_eq!(found, point(0, 0)..=point(0, 2));
        assert_eq!(search(&term, &mut regex, Some(&found), true), Some(found));
        assert!(compile("(x)").is_some());
        assert!(compile("").is_none());
        let mut missing = compile("absent").unwrap();
        assert_eq!(search(&term, &mut missing, None, true), None);
    }

    #[test]
    fn finds_text_across_a_soft_wrap_in_wide_cells_and_with_combining_marks() {
        let term = term(5, 5, "abcdgh\r\n\u{6f22}\u{5b57}\r\ne\u{301}".as_bytes());
        let find = |query: &str| search(&term, &mut compile(query).unwrap(), None, true);
        assert_eq!(find("cdgh"), Some(point(0, 2)..=point(1, 0)));
        assert_eq!(find("\u{6f22}\u{5b57}"), Some(point(2, 0)..=point(2, 3)));
        assert_eq!(find("e\u{301}"), Some(point(3, 0)..=point(3, 0)));
    }

    #[test]
    fn lists_only_the_matches_on_screen() {
        let term = term(20, 2, b"hit\r\nhit\r\nmiss\r\nhit");
        let mut regex = compile("hit").unwrap();
        let lines: Vec<i32> = visible(&term, &mut regex)
            .iter()
            .map(|found| found.start().line.0)
            .collect();
        assert_eq!(lines, [1]);
    }
}

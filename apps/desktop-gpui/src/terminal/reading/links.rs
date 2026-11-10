//! Web and file links in terminal text, and OSC 8 hyperlinks a program wrote.
//! Text detection is `terminal-interactions`' (shared with mobile); this maps it
//! onto the grid and decides what a click opens. Port of
//! `terminalLinkActivation.ts`, `activatedTerminalUrl`, and the link provider
//! and OSC 8 `linkHandler` in `TerminalRenderer.ts`.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Line, Point};
use gpui_kit::Modifiers;
use terminal_interactions::{LinkKind, terminal_links_for_buffer_line};

use crate::commands::registry::Platform;

use super::buffer::{GridBuffer, buffer_row, grid_line};

/// Rows read on each side of the one under the pointer. Detection joins at
/// most 128 rows of one logical line, and looks back as far again for an
/// application's hard-wrapped link.
const LINK_WINDOW: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    pub text: String,
    /// First and last cell, inclusive.
    pub start: Point,
    pub end: Point,
}

impl Link {
    pub fn contains(&self, point: Point) -> bool {
        self.start <= point && point <= self.end
    }
}

/// The link under `point`, if any. A hyperlink the program wrote wins over
/// text that looks like one, as in xterm.
pub fn link_at<T>(term: &Term<T>, point: Point) -> Option<Link> {
    hyperlink_at(term, point).or_else(|| {
        text_links(term, point.line)
            .into_iter()
            .find(|link| link.contains(point))
    })
}

/// The OSC 8 hyperlink under `point`: the run of cells around it that carry
/// the same one. It opens like a web link, so only `http(s)` targets leave.
pub fn hyperlink_at<T>(term: &Term<T>, point: Point) -> Option<Link> {
    let hyperlink = term.grid()[point].hyperlink()?;
    let same = |at: Point| term.grid()[at].hyperlink().as_ref() == Some(&hyperlink);
    let (mut start, mut end) = (point, point);
    loop {
        let previous = start.sub(term, Boundary::Grid, 1);
        if previous == start || !same(previous) {
            break;
        }
        start = previous;
    }
    loop {
        let next = end.add(term, Boundary::Grid, 1);
        if next == end || !same(next) {
            break;
        }
        end = next;
    }
    Some(Link {
        kind: LinkKind::Web,
        text: hyperlink.uri().to_owned(),
        start,
        end,
    })
}

/// The links in the text of one line, which may begin or end on another.
pub fn text_links<T>(term: &Term<T>, line: Line) -> Vec<Link> {
    let row = buffer_row(term, line);
    let buffer = GridBuffer::new(term, row.saturating_sub(LINK_WINDOW)..=row + LINK_WINDOW);
    terminal_links_for_buffer_line(&buffer, term.columns(), row + 1)
        .into_iter()
        .map(|link| {
            // Detection reports one-based, inclusive cells.
            let at = |position: terminal_interactions::Position| {
                Point::new(
                    grid_line(term, position.y - 1),
                    Column(position.x.saturating_sub(1)),
                )
            };
            Link {
                kind: link.kind,
                text: link.text,
                start: at(link.range.start),
                end: at(link.range.end),
            }
        })
        .collect()
}

/// What a line shows, to tell whether output changed it.
pub fn line_text<T>(term: &Term<T>, line: Line) -> String {
    let row = &term.grid()[line];
    (0..term.columns())
        .map(|column| row[Column(column)].c)
        .collect()
}

/// The deliberate modifier that opens a terminal link on each platform: a
/// plain click selects text like any other cell.
pub fn is_link_activation(modifiers: &Modifiers, platform: Platform) -> bool {
    match platform {
        Platform::Mac => modifiers.platform,
        Platform::Linux => modifiers.control,
    }
}

/// The URL a web link opens, or `None` for anything but `http(s)`. Normalized
/// the way a browser's `URL.href` is for the parts that matter here: the
/// scheme in lower case, and a bare host given its root path.
pub fn external_url(value: &str) -> Option<String> {
    let (scheme, rest) = value.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if authority_end == 0 {
        return None;
    }
    let (authority, tail) = rest.split_at(authority_end);
    let slash = if tail.starts_with('/') { "" } else { "/" };
    Some(format!("{scheme}://{authority}{slash}{tail}"))
}

#[cfg(test)]
mod tests {
    use super::super::buffer::tests::term;
    use super::super::selection::point;
    use super::*;

    fn modifiers(control: bool, platform: bool) -> Modifiers {
        Modifiers {
            control,
            platform,
            ..Modifiers::default()
        }
    }

    #[test]
    fn uses_the_platforms_own_modifier() {
        use Platform::{Linux, Mac};
        let cases = [
            (false, true, Mac, true),
            (true, false, Mac, false),
            (true, false, Linux, true),
            (false, true, Linux, false),
            (false, false, Mac, false),
            (false, false, Linux, false),
        ];
        for (control, command, platform, opens) in cases {
            assert_eq!(
                is_link_activation(&modifiers(control, command), platform),
                opens,
                "{control} {command} {platform:?}"
            );
        }
    }

    #[test]
    fn admits_only_http_and_https_links() {
        assert_eq!(
            external_url("https://example.com/x").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(
            external_url("http://localhost:3000").as_deref(),
            Some("http://localhost:3000/")
        );
        assert_eq!(
            external_url("HTTPS://example.com?q=1").as_deref(),
            Some("https://example.com/?q=1")
        );
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "not a url",
            "http:///x",
        ] {
            assert_eq!(external_url(refused), None, "{refused}");
        }
    }

    #[test]
    fn finds_the_web_link_under_the_pointer_across_a_soft_wrap() {
        let term = term(20, 3, b"see https://example.com/long/path ok");
        let link = link_at(&term, point(1, 2)).unwrap();
        assert_eq!(link.kind, LinkKind::Web);
        assert_eq!(link.text, "https://example.com/long/path");
        assert_eq!((link.start, link.end), (point(0, 4), point(1, 12)));
        assert!(link_at(&term, point(0, 1)).is_none());
    }

    #[test]
    fn finds_home_relative_and_relative_file_paths() {
        let term = term(40, 3, b"edit ~/notes/todo.md:12 or src/lib.rs");
        let home = link_at(&term, point(0, 8)).unwrap();
        assert_eq!(home.kind, LinkKind::File);
        assert_eq!(
            home.text, "~/notes/todo.md",
            "the location stays outside the link"
        );
        let relative = link_at(&term, point(0, 30)).unwrap();
        assert_eq!(relative.text, "src/lib.rs");
    }

    #[test]
    fn an_osc8_hyperlink_spans_its_text_and_carries_its_target() {
        let screen = "see \x1b]8;;https://example.com/pr/1\x1b\\PR #1\x1b]8;;\x1b\\ ok";
        let term = term(40, 2, screen.as_bytes());
        let link = link_at(&term, point(0, 6)).unwrap();
        assert_eq!(link.kind, LinkKind::Web);
        assert_eq!(link.text, "https://example.com/pr/1");
        assert_eq!((link.start, link.end), (point(0, 4), point(0, 8)));
        assert!(link_at(&term, point(0, 10)).is_none());
    }

    #[test]
    fn finds_a_link_in_the_scrollback() {
        let mut lines = String::from("open https://example.com/old\r\n");
        lines.push_str(&"filler\r\n".repeat(5));
        let term = term(40, 3, lines.as_bytes());
        assert!(term.grid().history_size() > 0);
        let top = -(term.grid().history_size() as i32);
        let link = link_at(&term, point(top, 10)).unwrap();
        assert_eq!(link.text, "https://example.com/old");
        assert_eq!(link.start.line.0, top);
    }
}

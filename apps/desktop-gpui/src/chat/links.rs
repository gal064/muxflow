//! File links in an agent's Markdown: a Markdown link to a path, and inline
//! code that is a path existing on the host. Both show the file's name and
//! open the full path. Fenced code is left as written.
//!
//! The text is rewritten before Kit's text view parses it: a file link's
//! target gets [`FILE_SCHEME`], which the view's link handler takes back off.

use std::fmt::Write as _;

/// Marks a link target as a path on the host, relative to the agent's
/// directory unless absolute.
pub const FILE_SCHEME: &str = "muxflow-file:";

/// Whether the host has a file at an inline code path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Pending,
    Exists,
    Missing,
}

/// The path a rewritten link opens, or `None` for any other link.
pub fn file_target(href: &str) -> Option<String> {
    let encoded = href.strip_prefix(FILE_SCHEME)?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        let decoded = (byte == b'%' && tail.len() >= 2)
            .then(|| std::str::from_utf8(&tail[..2]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(value) => {
                bytes.push(value);
                rest = &tail[2..];
            }
            None => {
                bytes.push(byte);
                rest = tail;
            }
        }
    }
    String::from_utf8(bytes).ok()
}

fn encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/._-~+@,=".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Splits `path:line` or `path:line:column` into the path and that suffix.
pub fn split_position(path: &str) -> (&str, &str) {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let mut end = path.len();
    for _ in 0..2 {
        match path[..end].rfind(':') {
            Some(colon) if digits(&path[colon + 1..end]) => end = colon,
            _ => break,
        }
    }
    path.split_at(end)
}

/// The path as the host's file resolution takes it: absolute, from the home
/// directory, or relative with a separator.
pub fn resolvable(path: &str) -> String {
    if path.contains('/') {
        path.to_owned()
    } else {
        format!("./{path}")
    }
}

/// Inline code that could name a file: one word with a separator in it. The
/// host says whether it does.
fn path_like(code: &str) -> bool {
    let (path, _) = split_position(code);
    path.len() <= 512
        && path.contains('/')
        && !path.ends_with('/')
        && !path.contains("//")
        && path.chars().any(char::is_alphabetic)
        && path
            .chars()
            .all(|c| c.is_alphanumeric() || "/._-~+@,".contains(c))
}

fn file_name(path: &str) -> &str {
    let (path, _) = split_position(path);
    path.rsplit('/').next().unwrap_or(path)
}

/// A Markdown link's target that is a path: no scheme, and not an anchor.
fn is_file_target(target: &str) -> bool {
    let scheme = target.split_once(':').is_some_and(|(scheme, rest)| {
        scheme.len() > 1
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            && !rest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b':')
    });
    !target.is_empty() && !scheme && !target.starts_with('#') && !target.starts_with("//")
}

/// A link starting at `text`'s `[`: its label, its target and its length.
fn parse_link(text: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 0usize;
    let mut in_code = false;
    let mut close = None;
    for (index, c) in text.char_indices() {
        match c {
            '`' => in_code = !in_code,
            '[' if !in_code => depth += 1,
            ']' if !in_code => {
                depth -= 1;
                if depth == 0 {
                    close = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = text[close + 1..].strip_prefix('(')?;
    // `<path with spaces>`, or a path and then a title.
    let (target, consumed) = match after.trim_start().strip_prefix('<') {
        Some(bracketed) => {
            let lead = after.len() - after.trim_start().len();
            let end = bracketed.find('>')?;
            let paren = bracketed[end + 1..].find(')')?;
            (&bracketed[..end], lead + 1 + end + 1 + paren + 1)
        }
        None => {
            let end = after.find(')')?;
            let target = after[..end].split_whitespace().next().unwrap_or("");
            (target, end + 1)
        }
    };
    Some((&text[1..close], target, close + 2 + consumed))
}

fn rewrite_line(line: &str, probe: &mut dyn FnMut(&str) -> Probe, out: &mut String) {
    let mut index = 0;
    while index < line.len() {
        let rest = &line[index..];
        if rest.starts_with('`') {
            let run = rest.bytes().take_while(|byte| *byte == b'`').count();
            let fence = &rest[..run];
            let Some(length) = rest[run..].find(fence) else {
                out.push_str(fence);
                index += run;
                continue;
            };
            let code = &rest[run..run + length];
            let whole = &rest[..run + length + run];
            if run == 1 && path_like(code) && probe(split_position(code).0) == Probe::Exists {
                let (_, position) = split_position(code);
                let _ = write!(
                    out,
                    "[`{}{position}`]({FILE_SCHEME}{})",
                    file_name(code),
                    encode(split_position(code).0),
                );
            } else {
                out.push_str(whole);
            }
            index += whole.len();
            continue;
        }
        let image = line[..index].ends_with('!');
        if rest.starts_with('[')
            && let Some((label, target, length)) = parse_link(rest)
        {
            if image {
                // Never fetched: its alt text stands in for it.
                out.pop();
                if !label.trim().is_empty() {
                    let _ = write!(out, "`{}`", label.trim());
                }
            } else if is_file_target(target) {
                let (path, position) = split_position(target);
                let shown = if label.contains('/') || label.trim().is_empty() {
                    format!("{}{position}", file_name(target))
                } else {
                    label.to_owned()
                };
                let _ = write!(out, "[{shown}]({FILE_SCHEME}{})", encode(path));
            } else {
                out.push_str(&rest[..length]);
            }
            index += length;
            continue;
        }
        let c = rest.chars().next().expect("index is inside the line");
        out.push(c);
        index += c.len_utf8();
    }
}

/// The Markdown to show for an agent's `markdown`. `probe` is asked about
/// each inline code path, and says [`Probe::Pending`] until the host has
/// answered: the code stays code until then.
pub fn rewrite(markdown: &str, probe: &mut dyn FnMut(&str) -> Probe) -> String {
    // The open fence's marker: a fence closes only on the same character, as long.
    let mut fence: Option<(char, usize)> = None;
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |m| trimmed.chars().take_while(|c| *c == m).count());
        match (fence, marker) {
            (None, Some(m)) if run >= 3 && !(m == '`' && trimmed[run..].contains('`')) => {
                fence = Some((m, run));
                out.push_str(line);
            }
            (Some((open, length)), _) => {
                if marker == Some(open) && run >= length && trimmed[run..].trim().is_empty() {
                    fence = None;
                }
                out.push_str(line);
            }
            _ => rewrite_line(line, probe, &mut out),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(markdown: &str, existing: &[&str]) -> String {
        rewrite(markdown, &mut |path| {
            if existing.contains(&path) {
                Probe::Exists
            } else {
                Probe::Missing
            }
        })
    }

    #[test]
    fn a_markdown_link_to_a_path_opens_the_path_and_keeps_its_label() {
        assert_eq!(
            shown("The join is in [links.rs](crates/term/src/links.rs).", &[]),
            "The join is in [links.rs](muxflow-file:crates/term/src/links.rs)."
        );
        assert_eq!(
            shown("See [the join](src/links.rs:171).", &[]),
            "See [the join](muxflow-file:src/links.rs)."
        );
    }

    #[test]
    fn a_link_labelled_with_its_path_shows_the_file_name() {
        assert_eq!(
            shown(
                "[crates/term/src/links.rs:12](crates/term/src/links.rs:12)",
                &[]
            ),
            "[links.rs:12](muxflow-file:crates/term/src/links.rs)"
        );
    }

    #[test]
    fn a_web_link_an_anchor_and_a_mail_link_are_left_alone() {
        for link in [
            "[docs](https://docs.rs/unicode-width)",
            "[top](#top)",
            "[me](mailto:me@example.com)",
            "[cdn](//cdn.example.com/x.js)",
        ] {
            assert_eq!(shown(link, &[]), link);
        }
    }

    #[test]
    fn inline_code_is_a_link_only_when_the_host_has_the_file() {
        let text = "The fixture is `tests/fixtures/boxed.txt`, not `tests/none.txt`.";
        assert_eq!(
            shown(text, &["tests/fixtures/boxed.txt"]),
            "The fixture is [`boxed.txt`](muxflow-file:tests/fixtures/boxed.txt), not `tests/none.txt`."
        );
    }

    #[test]
    fn a_line_number_is_shown_and_not_asked_about() {
        let mut asked = Vec::new();
        let out = rewrite("`src/a.rs:12:3`", &mut |path| {
            asked.push(path.to_owned());
            Probe::Exists
        });
        assert_eq!(asked, ["src/a.rs"]);
        assert_eq!(out, "[`a.rs:12:3`](muxflow-file:src/a.rs)");
    }

    #[test]
    fn code_that_is_no_path_is_never_asked_about() {
        let mut asked = 0;
        let text = "`cargo test`, `/`, `1/2`, `a//b`, `dir/`, `https://x.dev/a`, ``a/b``";
        let out = rewrite(text, &mut |_| {
            asked += 1;
            Probe::Exists
        });
        assert_eq!(asked, 0);
        assert_eq!(out, text);
    }

    #[test]
    fn fenced_code_is_left_as_written() {
        let text = "```sh\ncat `src/a.rs` [x](src/a.rs)\n```\n`src/a.rs`\n";
        assert_eq!(
            shown(text, &["src/a.rs"]),
            "```sh\ncat `src/a.rs` [x](src/a.rs)\n```\n[`a.rs`](muxflow-file:src/a.rs)\n"
        );
    }

    #[test]
    fn an_image_is_never_fetched() {
        assert_eq!(
            shown("![chart](https://x.dev/c.png) and ![](y.png)", &[]),
            "`chart` and "
        );
    }

    #[test]
    fn a_target_survives_its_encoding() {
        let out = shown("[notes](<docs/my notes (v2).md>)", &[]);
        let href = out
            .strip_prefix("[notes](")
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap();
        assert_eq!(file_target(href).as_deref(), Some("docs/my notes (v2).md"));
        assert_eq!(file_target("https://x.dev"), None);
    }

    #[test]
    fn a_bare_file_name_is_made_relative_for_the_host() {
        assert_eq!(resolvable("README.md"), "./README.md");
        assert_eq!(resolvable("docs/a.md"), "docs/a.md");
        assert_eq!(resolvable("/etc/hosts"), "/etc/hosts");
    }
}

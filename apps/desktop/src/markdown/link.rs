//! Where a link in the Markdown preview goes. Port of `markdownFilePath` in
//! `features/files/markdown.ts`, `isMarkdownFileLink` in `@muxflow/markdown`, and
//! the routing in `AppTabSurface.tsx`'s `onFileLink` and preview click handler.

/// What a clicked link asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// An `http(s)` URL, opened in the browser once the user has seen it.
    External(String),
    /// A file, resolved against the document: the host canonicalizes it and
    /// decides whether it may be read.
    File(String),
    /// Nothing to open (`mailto:`, `#section`, a malformed path): a status line.
    Status(String),
}

pub fn target(document_path: &str, href: &str) -> Target {
    let starts = |prefix: &str| {
        href.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    };
    if starts("http:") || starts("https:") {
        return Target::External(href.to_owned());
    }
    match file_path(document_path, href) {
        Ok(Some(path)) => Target::File(path),
        Ok(None) => Target::Status(format!("Markdown link: {href}")),
        Err(error) => Target::Status(format!("Could not open {href}: {error}")),
    }
}

/// File paths are inert links; schemes and network-path references are not.
fn is_file_link(href: &str) -> bool {
    let scheme = href.split_once(':').is_some_and(|(scheme, _)| {
        let mut chars = scheme.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    !href.is_empty()
        && href.trim() == href
        && !scheme
        && !href.starts_with("//")
        && !href.starts_with(['?', '#'])
        && !href.chars().any(|c| is_control(c) || c == '\\')
}

fn is_control(c: char) -> bool {
    c <= '\u{1f}' || c == '\u{7f}'
}

/// `decodeURIComponent`: a bad escape or a result that is not UTF-8 is an error.
fn decode(path: &str) -> Result<String, String> {
    let malformed = || "URI malformed".to_owned();
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'%' {
            out.push(bytes[at]);
            at += 1;
            continue;
        }
        let hex = path.get(at + 1..at + 3).ok_or_else(malformed)?;
        out.push(u8::from_str_radix(hex, 16).map_err(|_| malformed())?);
        at += 3;
    }
    String::from_utf8(out).map_err(|_| malformed())
}

/// Resolve against the document, leaving canonicalization and access to the host.
/// `None` is a link that names no file.
pub fn file_path(document_path: &str, href: &str) -> Result<Option<String>, String> {
    if !is_file_link(href) {
        return Ok(None);
    }
    let path = decode(href.split(['?', '#']).next().unwrap_or_default())?;
    if path.is_empty() || path.chars().any(is_control) {
        return Err("Invalid Markdown file path.".into());
    }
    if path.starts_with('/') {
        return Ok(Some(path));
    }
    let directory = document_path
        .rfind('/')
        .map_or("", |slash| &document_path[..=slash]);
    Ok(Some(format!("{directory}{path}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(document: &str, href: &str) -> Option<String> {
        file_path(document, href).unwrap()
    }

    #[test]
    fn resolves_links_against_the_document_rather_than_the_pane_cwd() {
        for (document, href, expected) in [
            (
                "/repo/todo.md",
                "todos/T003-pricing-usage-limits.md",
                "/repo/todos/T003-pricing-usage-limits.md",
            ),
            (
                "/repo/todos/T003.md",
                "../docs/03-pre-launch.md",
                "/repo/todos/../docs/03-pre-launch.md",
            ),
            ("/repo/docs/plan.md", "./other.md", "/repo/docs/./other.md"),
            ("/repo/docs/plan.md", "/repo/todo.md", "/repo/todo.md"),
            ("note.md", "other.md", "other.md"),
        ] {
            assert_eq!(resolved(document, href).as_deref(), Some(expected));
        }
    }

    #[test]
    fn decodes_filenames_and_separates_fragments_and_queries() {
        for (href, expected) in [
            ("my%20plan%23one.md#section", "/repo/my plan#one.md"),
            ("plan.md?raw=1", "/repo/plan.md"),
            ("plan.md#section", "/repo/plan.md"),
            ("caf%C3%A9.md", "/repo/café.md"),
            // Tauri refuses a raw backslash, not a decoded one.
            ("a%5Cb.md", "/repo/a\\b.md"),
        ] {
            assert_eq!(resolved("/repo/todo.md", href).as_deref(), Some(expected));
        }
    }

    #[test]
    fn leaves_external_and_same_document_links_alone_and_rejects_malformed_paths() {
        for href in [
            "https://example.com",
            "mailto:me@example.com",
            "#section",
            "?query",
            "//example.com/file",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,hello",
            " padded.md",
            "back\\slash.md",
            "",
        ] {
            assert_eq!(resolved("/repo/todo.md", href), None, "{href}");
        }
        for href in ["bad%00.md", "bad%XX.md", "bad%.md", "bad%E9.md", "bad%1"] {
            assert!(file_path("/repo/todo.md", href).is_err(), "{href}");
        }
    }

    #[test]
    fn routes_each_kind_of_link_to_what_it_opens() {
        let route = |href| target("/repo/docs/plan.md", href);
        assert_eq!(
            route("https://example.com/a"),
            Target::External("https://example.com/a".into())
        );
        assert_eq!(
            route("HTTP://example.com"),
            Target::External("HTTP://example.com".into())
        );
        assert_eq!(
            route("../todo.md#top"),
            Target::File("/repo/docs/../todo.md".into())
        );
        assert_eq!(
            route("#section"),
            Target::Status("Markdown link: #section".into())
        );
        assert_eq!(
            route("mailto:me@example.com"),
            Target::Status("Markdown link: mailto:me@example.com".into())
        );
        let Target::Status(error) = route("bad%XX.md") else {
            panic!("a malformed path opens nothing");
        };
        assert!(error.starts_with("Could not open bad%XX.md: "));
    }
}

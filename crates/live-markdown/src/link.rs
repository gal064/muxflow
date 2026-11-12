//! The link under a source position, for a ⌘-click (Ctrl on Linux). Port of
//! `linkAt` and `linkTarget` in `liveMarkdown.ts`.

use markdown::mdast::Node;

/// The target of the inline link, autolink or bare URL holding `pos`. An
/// image is not a link, and neither is a reference link: Tauri opens only a
/// link that names its destination.
pub fn link_at(ast: &Node, pos: usize) -> Option<String> {
    let mut node = ast;
    'descend: loop {
        for child in node.children().into_iter().flatten() {
            let Some(p) = child.position() else {
                continue;
            };
            if p.start.offset <= pos && pos < p.end.offset {
                match child {
                    Node::Link(link) => return Some(link_target(&link.url)),
                    Node::Image(_) | Node::ImageReference(_) => return None,
                    _ => {
                        node = child;
                        continue 'descend;
                    }
                }
            }
        }
        return None;
    }
}

/// A destination as the preview's renderer resolves it. The parser has
/// already dropped `<…>` and undone escapes; a bare `www.` link reads as http
/// and a bare address as mail.
pub fn link_target(url: &str) -> String {
    if url
        .get(..4)
        .is_some_and(|start| start.eq_ignore_ascii_case("www."))
    {
        return format!("http://{url}");
    }
    let bare = |part: &str| {
        !part.is_empty() && !part.contains(|c: char| c.is_whitespace() || "/:@".contains(c))
    };
    let email = url.split_once('@').is_some_and(|(user, domain)| {
        bare(user)
            && domain
                .split_once('.')
                .is_some_and(|(name, rest)| bare(name) && rest.split('.').all(bare))
    });
    if email {
        format!("mailto:{url}")
    } else {
        url.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::parse;

    // Mirrors liveMarkdown.test.ts "finds the target of a link, an autolink
    // and a bare URL".
    #[test]
    fn finds_the_target_of_a_link_an_autolink_and_a_bare_url() {
        let src = "[a](x.md) <https://a.dev> https://b.dev ![i](y.png)";
        let ast = parse(src);
        assert_eq!(link_at(&ast, 1).as_deref(), Some("x.md"));
        assert_eq!(link_at(&ast, 13).as_deref(), Some("https://a.dev"));
        assert_eq!(link_at(&ast, 30).as_deref(), Some("https://b.dev"));
        assert_eq!(link_at(&ast, 43), None);
        assert_eq!(link_at(&ast, 9), None, "just past a link is not in it");
    }

    #[test]
    fn a_reference_link_or_plain_text_opens_nothing() {
        let src = "See [the docs][ref] and **bold**.\n\n[ref]: ./a.md";
        let ast = parse(src);
        assert_eq!(link_at(&ast, 6), None);
        assert_eq!(link_at(&ast, 27), None);
    }

    #[test]
    fn finds_a_link_inside_emphasis_a_quote_a_list_or_a_table() {
        for src in [
            "**[a](x.md)**",
            "> - [a](x.md)",
            "| h |\n| - |\n| [a](x.md) |",
        ] {
            let at = src.find("[a]").unwrap() + 1;
            assert_eq!(link_at(&parse(src), at).as_deref(), Some("x.md"), "{src}");
        }
    }

    // Mirrors liveMarkdown.test.ts "linkTarget resolves a destination as the
    // preview does", through the parser that now does half of it.
    #[test]
    fn resolves_a_destination_as_the_preview_does() {
        let target = |src: &str| link_at(&parse(src), 1).unwrap();
        assert_eq!(target("[a](<x y.md>)"), "x y.md");
        assert_eq!(target("[a](<https://x.y>)"), "https://x.y");
        assert_eq!(target("[a](a\\(b\\).md)"), "a(b).md");
        assert_eq!(target("[a](www.example.com)"), "http://www.example.com");
        assert_eq!(target("[a](./plain.md)"), "./plain.md");
        assert_eq!(target("[a](me@example.com)"), "mailto:me@example.com");
        assert_eq!(link_target("WWW.example.com"), "http://WWW.example.com");
        assert_eq!(link_target("a@b"), "a@b");
    }
}

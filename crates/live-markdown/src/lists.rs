//! Lists as Tauri's parser (lezer) reads them. CommonMark lets a list item
//! break into a paragraph only when it has text and, if numbered, is numbered
//! one. Inside a list of the same kind lezer drops both rules, so the empty
//! item Enter then Tab makes, or a nested list that starts at 5, is an item
//! there rather than a heading's underline or more of the paragraph. This
//! parses the source with those lines made acceptable to CommonMark, then maps
//! the tree back to the source.

use markdown::mdast::Node;
use markdown::{ParseOptions, to_mdast};

pub fn parse(src: &str) -> Node {
    let ast = to_mdast(src, &ParseOptions::gfm()).expect("plain markdown cannot fail to parse");
    let mut patches = Vec::new();
    find(&ast, src, false, false, &mut patches);
    if patches.is_empty() {
        return ast;
    }
    // A number becomes `1`, its delimiter moved up and spaces after, so the
    // text keeps its column; an empty item holds an empty heading, which no
    // later line can join as a paragraph's would, taken out again below.
    let mut patched = String::with_capacity(src.len() + FILL.len() * patches.len());
    let mut fills = Vec::new();
    let mut at = 0;
    for Patch {
        marker,
        renumber,
        fill,
    } in patches
    {
        patched.push_str(&src[at..marker.start]);
        let delim = &src[marker.end - 1..marker.end];
        patched.push_str(if renumber {
            "1"
        } else {
            &src[marker.start..marker.end - 1]
        });
        patched.push_str(delim);
        if fill {
            fills.push(patched.len());
            patched.push_str(FILL);
        }
        if renumber {
            patched.push_str(&" ".repeat(marker.len() - 2));
        }
        at = marker.end;
    }
    patched.push_str(&src[at..]);
    let mut ast =
        to_mdast(&patched, &ParseOptions::gfm()).expect("plain markdown cannot fail to parse");
    unfill(&mut ast, &fills);
    ast
}

const FILL: &str = " #";

/// A list marker to rewrite: as `1` with the same delimiter, or with a word
/// after it, or both.
struct Patch {
    marker: std::ops::Range<usize>,
    renumber: bool,
    fill: bool,
}

/// The lines of paragraphs (and setext headings, whose underline may be an
/// empty `-` item) inside lists that lezer would start an item on.
fn find(node: &Node, src: &str, bullet: bool, ordered: bool, out: &mut Vec<Patch>) {
    for child in node.children().into_iter().flatten() {
        let (bullet, ordered) = match child {
            Node::List(l) => (bullet || !l.ordered, ordered || l.ordered),
            _ => (bullet, ordered),
        };
        let Some(p) = child.position() else {
            continue;
        };
        if !matches!(child, Node::Paragraph(_) | Node::Heading(_)) {
            find(child, src, bullet, ordered, out);
            continue;
        }
        if !bullet && !ordered {
            continue;
        }
        let mut line = p.start.offset;
        while let Some(nl) = src[line..p.end.offset].find('\n') {
            line += nl + 1;
            let text = &src[line..p.end.offset];
            let text = text.split('\n').next().unwrap_or_default();
            // A list in a quote has the quote's markers on every line.
            let mark = line + text.len() - text.trim_start_matches([' ', '\t', '>']).len();
            let rest = &src[mark..line + text.len()];
            let empty_after = |n: usize| {
                rest[n..].trim().is_empty()
                    && (rest.len() == n || rest[n..].starts_with([' ', '\t', '\r']))
            };
            if bullet && rest.starts_with(['-', '+', '*']) && empty_after(1) {
                out.push(Patch {
                    marker: mark..mark + 1,
                    renumber: false,
                    fill: true,
                });
                continue;
            }
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if !ordered
                || !(1..=9).contains(&digits)
                || !rest[digits..].starts_with(['.', ')'])
                || !(rest.len() == digits + 1 || rest[digits + 1..].starts_with([' ', '\t', '\r']))
            {
                continue;
            }
            let renumber = &rest[..digits] != "1";
            let fill = empty_after(digits + 1);
            if renumber || fill {
                out.push(Patch {
                    marker: mark..mark + digits + 1,
                    renumber,
                    fill,
                });
            }
        }
    }
}

/// Drops the heading each fill made and maps positions back to the source.
fn unfill(node: &mut Node, fills: &[usize]) {
    if let Some(children) = node.children_mut() {
        // The heading sits one byte into the fill, after its space.
        children.retain(|c| {
            !matches!(c, Node::Heading(_))
                || c.position()
                    .is_none_or(|p| !fills.iter().any(|f| f + 1 == p.start.offset))
        });
        for child in children {
            unfill(child, fills);
        }
    }
    if let Some(p) = node.position_mut() {
        p.start.offset = back(p.start.offset, fills);
        p.end.offset = back(p.end.offset, fills);
    }
}

/// The source position of a position in the filled text: one inside a fill
/// is where the fill went.
fn back(pos: usize, fills: &[usize]) -> usize {
    let mut shift = 0;
    for &fill in fills {
        if pos >= fill + FILL.len() {
            shift += FILL.len();
        } else if pos > fill {
            return fill - shift;
        }
    }
    pos - shift
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(node: &Node) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for child in node.children().into_iter().flatten() {
            if let Node::ListItem(item) = child {
                let p = child.position().unwrap();
                out.push((p.start.offset, p.end.offset, item.children.len()));
            }
            out.extend(items(child));
        }
        out
    }

    #[test]
    fn an_empty_item_in_a_list_is_an_item_not_an_underline_or_text() {
        let src = "- a\n  - ";
        assert_eq!(items(&parse(src)), vec![(0, 8, 2), (6, 8, 0)]);
        let src = "1. a\n   1. \n2. b";
        assert_eq!(
            items(&parse(src)),
            vec![(0, 11, 2), (8, 11, 0), (12, 16, 1)]
        );
        // Outside a list CommonMark stands: a lone `-` underlines a heading.
        assert!(matches!(
            parse("Title\n-").children().unwrap()[0],
            Node::Heading(_)
        ));
    }

    #[test]
    fn a_nested_ordered_list_may_start_at_any_number() {
        let src = "1. a\n   5. b\n   6. c";
        let ast = parse(src);
        assert_eq!(items(&ast), vec![(0, 20, 2), (8, 12, 1), (16, 20, 1)]);
        let mut text = Vec::new();
        collect_text(&ast, &mut text);
        assert_eq!(text, vec![(3, 4), (11, 12), (19, 20)]);
        // A bullet list does not let a number break in, as in lezer.
        assert_eq!(items(&parse("- a\n  5. b")).len(), 1);
    }

    #[test]
    fn positions_after_several_fills_map_back() {
        let src = "- a\n  - \n  - \n- b";
        let ast = parse(src);
        assert_eq!(
            items(&ast),
            vec![(0, 13, 2), (6, 8, 0), (11, 13, 0), (14, 17, 1)]
        );
    }

    fn collect_text(node: &Node, out: &mut Vec<(usize, usize)>) {
        if let Node::Text(_) = node {
            let p = node.position().unwrap();
            out.push((p.start.offset, p.end.offset));
        }
        for child in node.children().into_iter().flatten() {
            collect_text(child, out);
        }
    }
}

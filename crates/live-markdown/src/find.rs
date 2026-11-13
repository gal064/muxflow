//! What the view marks in the text it draws, as CodeMirror's search and
//! selection-match highlighters do: the find bar's matches, and the other
//! occurrences of the selected text. Only the drawn stretches are searched,
//! so a common word costs the same in a large file as in a small one. The
//! host's find bar does the matching that counts, steps and replaces; it
//! hands the view its compiled matcher.

use std::ops::Range;

use regex::Regex;

/// CodeMirror's `highlightSelectionMatches` defaults: a selection of up to
/// 200 characters, and nothing marked when more than 100 occurrences show.
const SELECTION_MAX_CHARS: usize = 200;
const SELECTION_MAX_MATCHES: usize = 100;

/// The matches of `matcher` that start in `spans` (sorted, in source bytes),
/// in order, read in the whole text as the bar reads it. An empty match is
/// not one, as the find bar counts. One pass over all the spans, so a query
/// with nothing after them reads the rest of the text once, not per span.
pub fn matches_in(text: &str, matcher: &Regex, spans: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (mut k, mut at) = (0, spans.first().map_or(0, |span| span.start));
    while let Some(found) = matcher.find_at(text, at) {
        while spans.get(k).is_some_and(|span| span.end <= found.start()) {
            k += 1;
        }
        let Some(span) = spans.get(k) else {
            break;
        };
        if found.start() < span.start {
            // Between two spans: on to the next.
            at = span.start;
        } else if found.is_empty() {
            let Some(c) = text[found.end()..].chars().next() else {
                break;
            };
            at = found.end() + c.len_utf8();
        } else {
            out.push(found.range());
            at = found.end();
        }
    }
    out
}

/// The other occurrences of the selected text in `spans`, as CodeMirror marks
/// them: taken literally and matching case, none that overlaps the selection,
/// and none at all past the limit.
pub fn selection_matches(
    text: &str,
    selection: &Range<usize>,
    spans: &[Range<usize>],
) -> Vec<Range<usize>> {
    let selected = &text[selection.clone()];
    // A long selection (Select All) is ruled out before counting its characters.
    let long = selected.len() > SELECTION_MAX_CHARS * 4;
    if selected.is_empty() || long || selected.chars().count() > SELECTION_MAX_CHARS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for span in spans {
        let mut end = (span.end + selected.len()).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        let haystack = &text[..end];
        let mut at = span.start;
        while let Some(offset) = haystack.get(at..).and_then(|rest| rest.find(selected)) {
            let found = at + offset..at + offset + selected.len();
            if found.start >= span.end {
                break;
            }
            if found.start >= selection.end || found.end <= selection.start {
                out.push(found.clone());
                if out.len() > SELECTION_MAX_MATCHES {
                    return Vec::new();
                }
            }
            at = found.end;
        }
    }
    out
}

#[cfg(test)]
// One drawn span is a slice of one range.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn word(text: &str) -> Regex {
        Regex::new(&format!(r"(?i)\b{text}\b")).unwrap()
    }

    #[test]
    fn finds_only_in_the_drawn_spans_with_the_whole_text_around_them() {
        let text = "cat\n\na cat\n\nconcat cat\n\ncat";
        let spans = [5..10, 12..22];
        assert_eq!(
            matches_in(text, &word("cat"), &spans),
            [7..10, 19..22],
            "the first and last are not drawn, and `concat` is no word"
        );
        // A match ending at a span's end still sees what follows it.
        assert!(matches_in("abc", &Regex::new(r"ab\b").unwrap(), &[0..2]).is_empty());
        // The stretches between spans are skipped.
        assert_eq!(
            matches_in("cat cat cat cat", &word("cat"), &[0..3, 12..15]),
            [0..3, 12..15]
        );
        // And a match may run on past it, as the bar counts it.
        assert_eq!(
            matches_in("cat\n\nx", &Regex::new("cat\n").unwrap(), &[0..3]),
            [0..4]
        );
        // Empty matches are skipped, and do not stop the search.
        assert_eq!(
            matches_in("axa", &Regex::new("x*").unwrap(), &[0..3]),
            [1..2]
        );
    }

    #[test]
    fn marks_the_selections_other_occurrences_literally_and_matching_case() {
        let text = "a.b Ab a.b a.b";
        assert_eq!(
            selection_matches(text, &(0..3), &[0..text.len()]),
            [7..10, 11..14]
        );
        assert!(selection_matches(text, &(1..1), &[0..text.len()]).is_empty());
        // Overlapping the selection is not another occurrence.
        assert_eq!(selection_matches("aaaa", &(0..2), &[0..4]), [2..4]);
    }

    #[test]
    fn marks_no_selection_matches_past_the_limits() {
        let many = "x ".repeat(SELECTION_MAX_MATCHES + 2);
        assert!(selection_matches(&many, &(0..1), &[0..many.len()]).is_empty());
        let few = "x ".repeat(SELECTION_MAX_MATCHES);
        assert_eq!(
            selection_matches(&few, &(0..1), &[0..few.len()]).len(),
            SELECTION_MAX_MATCHES - 1
        );
        let long = "y".repeat(SELECTION_MAX_CHARS + 1);
        let text = format!("{long} {long}");
        assert!(selection_matches(&text, &(0..long.len()), &[0..text.len()]).is_empty());
    }
}

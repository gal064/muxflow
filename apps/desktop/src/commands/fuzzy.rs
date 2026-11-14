//! Subsequence matching for the palettes. Port of
//! the Tauri app's `commands/fuzzy.ts`.

const CONSECUTIVE_BONUS: i64 = 8;
const WORD_START_BONUS: i64 = 6;
const PREFIX_BONUS: i64 = 12;
const GAP_PENALTY: i64 = 1;

#[derive(Debug, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i64,
    /// Char positions in the haystack that matched, for highlighting.
    pub indices: Vec<usize>,
}

fn lower(character: char) -> char {
    character.to_lowercase().next().unwrap_or(character)
}

fn is_word_boundary(target: &[char], index: usize) -> bool {
    matches!(target[index - 1], ' ' | '/' | '-' | '_' | '.' | ':')
}

pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<FuzzyMatch> {
    if needle.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }
    let target: Vec<char> = haystack.chars().map(lower).collect();
    let mut indices = Vec::new();
    let mut score = 0;
    let mut cursor = 0;
    let mut previous: i64 = -2;
    // Whitespace in the query is a separator, not something to match.
    for character in needle
        .chars()
        .filter(|character| *character != ' ')
        .map(lower)
    {
        let found = cursor + target[cursor..].iter().position(|c| *c == character)?;
        let position = found as i64;
        if position == previous + 1 {
            score += CONSECUTIVE_BONUS;
        } else {
            score -= (position - previous - 1).min(10) * GAP_PENALTY;
        }
        if found == 0 {
            score += PREFIX_BONUS;
        } else if is_word_boundary(&target, found) {
            score += WORD_START_BONUS;
        }
        indices.push(found);
        previous = position;
        cursor = found + 1;
    }
    // A short haystack that matched is more likely to be what was meant.
    Some(FuzzyMatch {
        score: score - (target.len() / 12) as i64,
        indices,
    })
}

/// Filters and ranks, keeping the input order for equal scores.
pub fn fuzzy_rank<T: Clone>(items: &[T], needle: &str, text: impl Fn(&T) -> String) -> Vec<T> {
    if needle.trim().is_empty() {
        return items.to_vec();
    }
    let mut ranked: Vec<(i64, &T)> = items
        .iter()
        .filter_map(|item| Some((fuzzy_match(&text(item), needle)?.score, item)))
        .collect();
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    ranked.into_iter().map(|(_, item)| item.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top<'a>(items: &[&'a str], needle: &str) -> &'a str {
        fuzzy_rank(items, needle, |item| (*item).to_owned())[0]
    }

    #[test]
    fn matches_in_order_with_gaps() {
        for needle in ["spl", "sright", "spr"] {
            assert!(fuzzy_match("Split pane right", needle).is_some());
        }
        assert!(fuzzy_match("Split pane right", "xyz").is_none());
        assert!(fuzzy_match("Split pane right", "tips").is_none());
    }

    #[test]
    fn ranks_the_command_a_human_meant() {
        let commands = [
            "Split pane right",
            "Show command palette",
            "Scroll terminal to bottom",
            "Configure keyboard shortcuts",
        ];
        assert_eq!(top(&commands, "spl"), "Split pane right");
        assert_eq!(top(&commands, "scroll"), "Scroll terminal to bottom");
        assert_eq!(
            top(&["Terminal reset", "Rename terminal tab"], "rt"),
            "Rename terminal tab"
        );
    }

    #[test]
    fn an_empty_query_keeps_everything_in_order() {
        let items = ["b", "a", "c"];
        for needle in ["", "   "] {
            assert_eq!(fuzzy_rank(&items, needle, |item| (*item).to_owned()), items);
        }
        assert_eq!(
            fuzzy_match("anything", ""),
            Some(FuzzyMatch {
                score: 0,
                indices: vec![]
            })
        );
    }

    #[test]
    fn reports_where_it_matched() {
        let indices = |needle| fuzzy_match("Split pane", needle).unwrap().indices;
        assert_eq!(indices("sp"), [0, 1]);
        assert_eq!(indices("s p"), [0, 1]);
        assert_eq!(indices("spane"), [0, 1, 7, 8, 9]);
    }

    #[test]
    fn is_case_insensitive_in_both_directions() {
        assert!(fuzzy_match("Split Pane", "SPLIT").is_some());
        assert!(fuzzy_match("SPLIT PANE", "split").is_some());
    }

    #[test]
    fn non_ascii_haystacks_match_by_character() {
        let matched = fuzzy_match("İstanbul/ü", "ü").unwrap();
        assert_eq!(matched.indices, [9]);
    }
}

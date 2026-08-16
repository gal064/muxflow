/**
 * Subsequence matching for the palettes.
 *
 * The old palette did a plain substring test on `group + title`, so "spl" found
 * "Split pane right" but "sright" found nothing and "newws" found nothing. This
 * matches characters in order with gaps allowed, and scores so that the match
 * a human meant sorts first: consecutive runs beat scattered hits, and a hit at
 * the start of a word beats one in the middle of one.
 */

const CONSECUTIVE_BONUS = 8;
const WORD_START_BONUS = 6;
const PREFIX_BONUS = 12;
const GAP_PENALTY = 1;

export interface FuzzyMatch {
  score: number;
  /** Indices in the haystack that matched, for highlighting. */
  indices: number[];
}

export function fuzzyMatch(haystack: string, needle: string): FuzzyMatch | undefined {
  if (needle === "") return { score: 0, indices: [] };
  const target = haystack.toLocaleLowerCase();
  const query = needle.toLocaleLowerCase();
  const indices: number[] = [];
  let score = 0;
  let cursor = 0;
  let previous = -2;
  for (const character of query) {
    // Whitespace in the query is a separator, not something to match.
    if (character === " ") continue;
    const found = target.indexOf(character, cursor);
    if (found < 0) return undefined;
    if (found === previous + 1) score += CONSECUTIVE_BONUS;
    else score -= Math.min(found - previous - 1, 10) * GAP_PENALTY;
    if (found === 0) score += PREFIX_BONUS;
    else if (isWordBoundary(target, found)) score += WORD_START_BONUS;
    indices.push(found);
    previous = found;
    cursor = found + 1;
  }
  // A short haystack that matched is more likely to be what was meant than a
  // long one that happened to contain the same letters.
  return { score: score - Math.floor(target.length / 12), indices };
}

function isWordBoundary(value: string, index: number): boolean {
  const before = value[index - 1];
  return before === " " || before === "/" || before === "-" || before === "_" || before === "." || before === ":";
}

/** Filters and ranks, keeping the input order for equal scores. */
export function fuzzyRank<T>(items: readonly T[], needle: string, text: (item: T) => string): T[] {
  if (needle.trim() === "") return [...items];
  return items
    .map((item, index) => ({ item, index, match: fuzzyMatch(text(item), needle) }))
    .filter((candidate): candidate is { item: T; index: number; match: FuzzyMatch } => candidate.match !== undefined)
    .sort((left, right) => right.match.score - left.match.score || left.index - right.index)
    .map((candidate) => candidate.item);
}

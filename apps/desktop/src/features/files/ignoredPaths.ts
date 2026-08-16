import type { GitStatusSnapshot } from "../git/types";

/**
 * What the Explorer may hide, derived from a git status snapshot.
 *
 * `undefined` means "show everything", and it is the answer to every question
 * short of a real one. A status that gave up — oversized, or a repository that
 * could not be walked — reports zero entries with `authoritative: false`, and
 * reading that as "nothing is ignored" would be right by accident while
 * reading it as truth would empty the tree on the first large repository. A
 * root that is not a worktree never produces a snapshot at all.
 *
 * Paths are absolute, rebuilt from `displayPath` — the repo-relative,
 * canonical spelling — against the worktree root. The `path` field is an
 * opaque identity and is never decoded.
 */
export function ignoredPathsFromStatus(status: GitStatusSnapshot | undefined): ReadonlySet<string> | undefined {
  if (!status?.authoritative || status.oversized) return undefined;
  const worktreeRoot = status.repository.worktreeRoot.replace(/\/+$/u, "");
  const paths = new Set<string>();
  for (const entry of status.entries) {
    if (!entry.ignored || hostOwnsThisName(entry.displayPath)) continue;
    paths.add(`${worktreeRoot}/${entry.displayPath}`);
  }
  return paths;
}

/**
 * Names whose visibility the host has already decided, which this filter must
 * not decide again.
 *
 * Two different reasons, one rule. `node_modules` is git-ignored in almost
 * every JavaScript repository, and the host deliberately *shows* it — collapsed
 * rather than hidden, because "a directory people open on purpose" is not the
 * same as build output (`filesystem.rs`, `COLLAPSED_DIRECTORIES`). Letting a
 * git-ignored-entry filter hide it anyway would overturn that decision from the
 * other side of the codebase, which is exactly what it did until this was
 * added. The rest are the host's `ALWAYS_HIDDEN` set: the listing never reports
 * them, so naming them here hides nothing and only inflates the set that
 * decides whether to offer "Show ignored files" — a toggle that changes nothing
 * is worse than no toggle.
 *
 * Kept as names rather than paths because both host rules are name rules: they
 * apply at every depth, and git reports `node_modules` once per occurrence.
 */
const HOST_OWNED_NAMES: ReadonlySet<string> = new Set([
  "node_modules",
  ".git",
  ".svn",
  ".hg",
  "CVS",
  ".DS_Store",
  "Thumbs.db",
]);

function hostOwnsThisName(displayPath: string): boolean {
  return HOST_OWNED_NAMES.has(displayPath.slice(displayPath.lastIndexOf("/") + 1));
}

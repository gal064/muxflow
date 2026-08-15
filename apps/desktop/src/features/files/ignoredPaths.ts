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
  for (const entry of status.entries) if (entry.ignored) paths.add(`${worktreeRoot}/${entry.displayPath}`);
  return paths;
}

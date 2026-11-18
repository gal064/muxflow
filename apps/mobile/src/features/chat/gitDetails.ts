// What Details says about the chat's folder (docs/design/mobile-chat-plan.md
// §3 "Git rows"): its name, the branch, and the two change counts. The rules
// are the desktop's (apps/desktop-gpui/src/chat/details.rs). Nothing here
// talks to a connection; `useGitDetails` does that.

import { ChatTurnState } from "../../protocol/gen/chat_pb";

/** One answer from the host: on its way, here, or not coming. */
export type GitRead<T> = { status: "reading" } | { status: "ready"; value: T } | { status: "failed" };

/** The part of a `GitBranchSummary` the rows show. */
export interface GitSummary {
  /** What it was compared with: `origin/main`, `HEAD`, or empty when there is no default branch. */
  baseRef: string;
  files: bigint;
  added: bigint;
  removed: bigint;
}

export interface GitDetails {
  /**
   * `unknown` until the host has said where the folder's root is, and again
   * when it could not; `none` when the folder is in no Git repository; `git`
   * with the worktree's `root`.
   */
  repository: "unknown" | "none" | "git";
  root: string;
  /** The current branch; empty on a detached HEAD. */
  branch: GitRead<string>;
  /** Against the merge base with the default branch. */
  changes: GitRead<GitSummary>;
  /** Against HEAD. */
  uncommitted: GitRead<GitSummary>;
}

const READING = { status: "reading" } as const;

/** Nothing known: no Git row is drawn. */
export const NO_GIT_DETAILS: GitDetails = { repository: "unknown", root: "", branch: READING, changes: READING, uncommitted: READING };

/** The folder is in no Git repository. */
export const NOT_A_REPOSITORY: GitDetails = { ...NO_GIT_DETAILS, repository: "none" };

/**
 * The details once the folder's worktree is known to be at `root`. What was
 * read for the same root stays on screen until the new answers replace it.
 */
export function inRepository(previous: GitDetails, root: string): GitDetails {
  if (previous.repository === "git" && previous.root === root) return previous;
  return { ...NO_GIT_DETAILS, repository: "git", root };
}

/** A turn that was working or waiting has ended: what it changed can be read. */
export function turnEnded(previous: ChatTurnState, next: ChatTurnState): boolean {
  return next === ChatTurnState.IDLE && (previous === ChatTurnState.WORKING || previous === ChatTurnState.WAITING);
}

// ---- the folder ---------------------------------------------------------------

/** A home folder as hosts lay them out. */
const HOME_SHAPE = /^(\/home\/[^/]+|\/Users\/[^/]+|\/root)(?:\/|$)/;

/** Whether the folder `path` is the worktree at `root` or lies in it. */
function within(path: string, root: string): boolean {
  const base = root.replace(/\/+$/, "");
  if (base === "" || !path.startsWith(base)) return false;
  const rest = path.slice(base.length);
  return rest === "" || rest.startsWith("/");
}

/**
 * The chat's folder as Details names it: inside the repository at `repoRoot`,
 * the repository's folder and the path within it (`muxflow/apps/mobile`);
 * else relative to the home the path starts with (`~/notes`). A folder
 * outside both stays as it is.
 */
export function folderLabel(path: string, repoRoot?: string | undefined): string {
  if (repoRoot && within(path, repoRoot)) {
    const root = repoRoot.replace(/\/+$/, "");
    return `${root.slice(root.lastIndexOf("/") + 1)}${path.slice(root.length)}`;
  }
  const home = HOME_SHAPE.exec(path)?.[1];
  return home ? `~${path.slice(home.length)}` : path;
}

// ---- the rows -------------------------------------------------------------------

/** `origin/main` reads as `main`. */
export function defaultBranchName(baseRef: string): string {
  const slash = baseRef.indexOf("/");
  return slash < 0 ? baseRef : baseRef.slice(slash + 1);
}

/** What trails a changes row. */
export type ChangeCount =
  /** Not read yet: `…`. */
  | { kind: "reading" }
  /** Nothing changed: `none`, the row dim. */
  | { kind: "none" }
  /** `+12`, `−3` and `2 files`. */
  | { kind: "lines"; added: string; removed: string; files: string };

export interface ChangesRow {
  /** "Changes vs main", "Uncommitted". */
  title: string;
  count: ChangeCount;
  /** The row as one sentence, for a screen reader. */
  label: string;
}

export interface GitRows {
  /** The branch's name, "Detached HEAD", or `…` while it is read. */
  branch?: string;
  changes?: ChangesRow;
  uncommitted?: ChangesRow;
}

export const READING_MARK = "\u2026";

function counted(summary: GitSummary): ChangeCount {
  if (summary.files === 0n) return { kind: "none" };
  return {
    kind: "lines",
    added: `+${summary.added}`,
    // U+2212, as the desktop writes it.
    removed: `\u2212${summary.removed}`,
    files: summary.files === 1n ? "1 file" : `${summary.files} files`,
  };
}

function row(title: string, count: ChangeCount): ChangesRow {
  const said = count.kind === "reading" ? READING_MARK : count.kind === "none" ? "none" : `${count.added} ${count.removed}, ${count.files}`;
  return { title, count, label: `${title}: ${said}` };
}

/** "Changes vs main". Left out when its read failed, with no default branch, and with nothing on the branch. */
function changesRow(read: GitRead<GitSummary>): ChangesRow | undefined {
  if (read.status === "failed") return undefined;
  if (read.status === "reading") return row("Changes", { kind: "reading" });
  const summary = read.value;
  if (summary.baseRef === "" || summary.files === 0n) return undefined;
  return row(`Changes vs ${defaultBranchName(summary.baseRef)}`, counted(summary));
}

/** "Uncommitted". Left out when its read failed. */
function uncommittedRow(read: GitRead<GitSummary>): ChangesRow | undefined {
  if (read.status === "failed") return undefined;
  if (read.status === "reading") return row("Uncommitted", { kind: "reading" });
  return row("Uncommitted", counted(read.value));
}

/** The Git rows Details draws. None outside a repository, or before the host has said there is one. */
export function gitRows(details: GitDetails): GitRows {
  if (details.repository !== "git") return {};
  const rows: GitRows = {};
  if (details.branch.status === "reading") rows.branch = READING_MARK;
  else if (details.branch.status === "ready") rows.branch = details.branch.value || "Detached HEAD";
  const changes = changesRow(details.changes);
  if (changes) rows.changes = changes;
  const uncommitted = uncommittedRow(details.uncommitted);
  if (uncommitted) rows.uncommitted = uncommitted;
  return rows;
}

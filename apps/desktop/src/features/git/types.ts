import type { ActiveRoot, FileWorkspaceScope } from "../files/types";

export type GitChangeKind = "none" | "modified" | "added" | "deleted" | "renamed" | "copied" | "typeChanged" | "unmerged" | "untracked" | "ignored";
export type GitDiffTarget = "staged" | "unstaged";
export type GitMutationKind = "stageFile" | "unstageFile" | "discardFile" | "stageHunk" | "unstageHunk" | "discardHunk";

export interface GitRepository {
  id: string;
  worktreeRoot: string;
  initial: boolean;
  detachedHead: boolean;
  headName?: string;
  headOid?: string;
}

export interface GitStatusEntry {
  /** Opaque base64 path identity. Never reconstruct this from displayPath. */
  path: string;
  displayPath: string;
  /**
   * The repository's worktree root joined to `path`, derived here rather than
   * carried per entry: the host already names that root once per snapshot, and
   * repeating it on every entry inflated the encoded status against a bound
   * that drops *every* entry once exceeded. Absent for non-UTF-8 Git paths.
   */
  absolutePath?: string;
  originalPath?: string;
  displayOriginalPath?: string;
  indexKind: GitChangeKind;
  worktreeKind: GitChangeKind;
  indexStatus: string;
  worktreeStatus: string;
  conflicted: boolean;
  conflictCode?: string;
  untracked: boolean;
  ignored: boolean;
  submodule: boolean;
  submoduleState?: string;
  symlink: boolean;
  binary: boolean;
  renameScore?: string;
}

export interface GitStatusSnapshot {
  repository: GitRepository;
  generation: string;
  sourceGeneration: string;
  entries: GitStatusEntry[];
  authoritative: boolean;
  oversized?: boolean;
  totalEntryCount?: string;
  error?: string;
  copyDetectionIncomplete?: boolean;
}

/// A diff body the control response deliberately withheld because it is large
/// enough to delay terminal traffic. Read over the bulk lane instead.
export interface GitDiffContentRef {
  /** Decimal u64 string. */
  size: string;
  contentDigest: string;
}

export interface GitDiff {
  repository: GitRepository;
  target: GitDiffTarget;
  path: string;
  originalPath?: string;
  displayPath: string;
  oldContent?: Uint8Array;
  newContent?: Uint8Array;
  oldContentRef?: GitDiffContentRef;
  newContentRef?: GitDiffContentRef;
  sourceGeneration: string;
  binary: boolean;
  tooLarge: boolean;
  oldMissing: boolean;
  newMissing: boolean;
  hunkCount: number;
}

/// A diff and the authoritative status it was read against, in one round trip.
export interface GitDiffResult {
  diff: GitDiff;
  status: GitStatusSnapshot;
}

export interface GitMutationRequest {
  kind: GitMutationKind;
  path: string;
  originalPath?: string;
  expectedStatusGeneration: string;
  expectedSourceGeneration: string;
  target: GitDiffTarget;
  hunkIndex?: number;
  confirmationToken?: string;
}

export interface GitCommandResult {
  exitCode: number;
  stdout: string;
  stderr: string;
  applied: boolean;
  refreshFailed: boolean;
  refreshError: string;
  outcome?: "notApplied" | "applied" | "partialOrUnknown" | "unspecified";
  stdoutTruncated?: boolean;
  stderrTruncated?: boolean;
  error?: string;
  preHeadOid?: string;
  postHeadOid?: string;
  preIndexGeneration?: string;
  postIndexGeneration?: string;
  postStateAuthoritative?: boolean;
  preStatusGeneration?: string;
  postStatusGeneration?: string;
  statusOmitted?: boolean;
  status?: GitStatusSnapshot;
}

export type GitWorkspaceEvent =
  | { kind: "status"; rootToken: string; watchId?: string; status: GitStatusSnapshot }
  | { kind: "error"; rootToken: string; watchId?: string; error: string };

export interface GitWatchLease {
  watchId: string;
  rootToken: string;
  connectionEpoch: number;
  status: GitStatusSnapshot;
  release(): void;
}

export interface GitWorkspaceClient {
  status(scope: FileWorkspaceScope, root: ActiveRoot, signal?: AbortSignal): Promise<GitStatusSnapshot>;
  watch(scope: FileWorkspaceScope, root: ActiveRoot, signal?: AbortSignal): Promise<GitWatchLease>;
  diff(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, path: string, originalPath: string | undefined, target: GitDiffTarget, signal?: AbortSignal): Promise<GitDiffResult>;
  prepareDiscard(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, request: GitMutationRequest): Promise<string>;
  mutate(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, request: GitMutationRequest): Promise<GitCommandResult>;
  commit(scope: FileWorkspaceScope, root: ActiveRoot, repositoryId: string, expectedStatusGeneration: string, message: string): Promise<GitCommandResult>;
  subscribe(listener: (event: GitWorkspaceEvent) => void): () => void;
}

import type { TransferCleanupStatus, TransferFailureKind, TransferOutcome, TransferState } from "../transfers/transferState";

export const TEXT_FILE_LIMIT_BYTES = 10 * 1024 * 1024;
export const IMAGE_PREVIEW_LIMIT_BYTES = 25 * 1024 * 1024;

export interface FileWorkspaceScope {
  clientId: string;
  hostProfileId: string;
  serverIdentity: string;
  generation: number;
  /** Host-issued generation epoch for this exact terminal bridge. */
  terminalEpoch: number;
  sessionId: string;
  paneId: string;
}

/**
 * The workspace a file surface represents, independent of a particular
 * transport connection. A reconnect replaces `clientId` and `terminalEpoch`
 * but does not replace this identity, so last-known content can remain visible
 * while the new bridge becomes writable.
 */
export type FileWorkspaceSelection = Pick<
  FileWorkspaceScope,
  "hostProfileId" | "serverIdentity" | "sessionId" | "paneId"
>;

export interface ActiveRoot {
  token: string;
  paneId: string;
  cwd: string;
  path: string;
  gitWorktree: boolean;
  revision: string;
}

/**
 * Host-issued capability for one terminal-linked file outside the pane root.
 *
 * It deliberately looks unlike an ordinary root token so a persisted file tab
 * can recover its no-directory-watch behavior after reconnecting, without
 * storing a second piece of capability metadata that could drift from the
 * token which actually enforces it.
 *
 * The token may save the exact linked file but cannot enumerate its parent or
 * access a sibling.
 */
export const TERMINAL_SINGLE_FILE_TOKEN_PREFIX = "file-v2:";

export function isTerminalSingleFileRoot(root: Pick<ActiveRoot, "token"> | undefined): boolean {
  return root?.token.startsWith(TERMINAL_SINGLE_FILE_TOKEN_PREFIX) ?? false;
}

export interface TerminalFileResolution {
  path: string;
  root: ActiveRoot;
  /** Decimal topology generation from the host's final pane revalidation. */
  topologyGeneration: string;
}

export interface TerminalFilePaneRoute {
  sessionId: string;
  windowId: string;
  cwd: string;
}

/** The narrow file capability used by terminal-link navigation. */
export interface TerminalFileResolver {
  resolveTerminalFile(
    scope: FileWorkspaceScope,
    path: string,
    pane: TerminalFilePaneRoute,
  ): Promise<TerminalFileResolution>;
}

export type FileKind = "file" | "directory" | "symlink";

export interface FileEntry {
  path: string;
  name: string;
  kind: FileKind;
  sizeBytes: string;
  modifiedMillis: string;
  /**
   * The host's exact version identity for this entry.
   *
   * Carried into the tree because a watch bootstrap is also the authoritative
   * answer to "is the file I just read still the file on disk?" — without it an
   * editor could only re-read unconditionally to find out, which is a second
   * remote round trip per open that almost always confirms what it already had.
   */
  generation: string;
  executable: boolean;
  symlinkTarget?: string;
  targetKind?: "file" | "directory" | "missing" | "other";
  expandable: boolean;
}

export interface DirectoryListing {
  rootToken: string;
  directory: string;
  revision: string;
  entries: FileEntry[];
  /**
   * The host rebuilt this listing because its watcher lost events, so anything
   * cached *below* this directory may have missed changes too.
   *
   * Distinct from `complete`, which is only about whether more pages follow.
   * One flag answering both questions could answer neither on its own.
   */
  recoveredFromOverflow: boolean;
  nextPageToken?: string;
  complete: boolean;
}

export interface DirectoryWatchLease {
  snapshot: DirectoryListing;
  /**
   * Whether `snapshot` describes the directory as of *this* acquisition.
   *
   * One host watch is shared by every subscriber, and its bootstrap listing is
   * produced once — when the watch was armed. A subscriber that joins an
   * already-established watch is therefore handed a listing of arbitrary age:
   * it can predate the join by the whole lifetime of the watch. Such a snapshot
   * is a paint, never an authority. It must be revalidated, and no freshness
   * decision may be taken from it.
   */
  fresh: boolean;
  release(): void;
}

export interface TextFile {
  path: string;
  content: string;
  generation: string;
  sizeBytes: string;
  lineEnding: "lf" | "crlf" | "mixed" | "none";
  encoding: "utf-8";
}

export interface BinaryFile {
  path: string;
  generation: string;
  sizeBytes: string;
  mime: string;
  previewKind: "image" | "binary";
  /** Bounded bytes streamed on the independent bulk lane; never sent on control. */
  previewBytes?: Uint8Array;
}

export type OpenFile = { kind: "text"; file: TextFile } | { kind: "binary"; file: BinaryFile };

export type FileMutation =
  | { kind: "createFile"; parent: string; name: string }
  | { kind: "createDirectory"; parent: string; name: string }
  | { kind: "rename"; path: string; destination: string; overwrite: boolean; confirmedNonEmpty: boolean }
  | { kind: "move"; path: string; destination: string; overwrite: boolean; confirmedNonEmpty: boolean }
  | { kind: "duplicate"; path: string; destination: string; overwrite: boolean; confirmedNonEmpty: boolean }
  | { kind: "delete"; path: string; confirmedNonEmpty: boolean };

export interface WriteTextRequest {
  path: string;
  content: string;
  baseGeneration: string;
  operationId: string;
  lineEnding: TextFile["lineEnding"];
}

export interface WriteTextResult {
  path: string;
  generation: string;
  operationId: string;
  sizeBytes: string;
}

export type CollisionPolicy = "fail" | "overwrite" | "rename";
export interface DownloadRequest {
  path: string;
  kind: "file" | "folder";
  destination?: string;
  collision: CollisionPolicy;
}

export interface TransferStatus {
  id: string;
  scopeKey: string;
  path: string;
  destination?: string;
  kind: "file" | "folder";
  state: TransferState;
  outcome?: TransferOutcome;
  failureKind?: TransferFailureKind;
  completedBytes: string;
  totalBytes?: string;
  filesCompleted: string;
  filesTotal?: string;
  bytesPerSecond?: string;
  etaSeconds?: number;
  digest?: string;
  error?: string;
  cleanupError?: string;
  cleanupStatus?: TransferCleanupStatus;
}

/**
 * What the host said, mapped, rather than reduced to "something changed".
 *
 * A precise event carries the exact entry it is about and an authoritative
 * rescan carries the whole listing, so the Explorer can patch or replace what
 * it already holds. Reducing either to an invalidation is what made one remote
 * file write cost a full directory list — over SSH, a round trip and the whole
 * payload again, to learn one row.
 */
export type WorkspaceEvent =
  | { kind: "rootChanged"; root: ActiveRoot }
  | { kind: "directorySnapshot"; rootToken: string; listing: DirectoryListing }
  | { kind: "fileChanged"; rootToken: string; path: string; generation: string; operationId?: string; entry?: FileEntry }
  | { kind: "fileDeleted"; rootToken: string; path: string }
  | { kind: "transfer"; transfer: TransferStatus };

export interface ListDirectoryOptions {
  pageToken?: string;
  /**
   * Aborting propagates a real cancellation to the bounded host scan *and*
   * rejects this call with an `AbortError`, like every other read on this
   * interface. A read that resolved anyway could still install rows into a
   * directory the tree had already given up on.
   */
  signal?: AbortSignal;
}

export interface AcquireWatchOptions {
  /**
   * Aborting stops the bootstrap listing on the host.
   *
   * The bootstrap *is* the expansion's listing, so a folder opened and closed
   * again on a slow link must stop the enumeration it started rather than pay
   * for it and discard the answer. Only the caller that starts a shared watch
   * can cancel it; a later subscriber joins an answer already in flight.
   */
  signal?: AbortSignal;
}

export interface ResolveRootOptions {
  /**
   * The root capability the caller already holds. The host answers an
   * unchanged root without a second authoritative discovery and without
   * broadcasting a duplicate ActiveRoot payload.
   */
  knownRootToken?: string;
}

export interface FileWorkspaceClient {
  resolveActiveRoot(scope: FileWorkspaceScope, options?: ResolveRootOptions): Promise<ActiveRoot>;
  listDirectory(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, options?: ListDirectoryOptions): Promise<DirectoryListing>;
  acquireDirectoryWatch(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, options?: AcquireWatchOptions): Promise<DirectoryWatchLease>;
  openFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, signal?: AbortSignal): Promise<OpenFile>;
  writeText(scope: FileWorkspaceScope, root: ActiveRoot, request: WriteTextRequest): Promise<WriteTextResult>;
  mutate(scope: FileWorkspaceScope, root: ActiveRoot, mutation: FileMutation): Promise<void>;
  startDownload(scope: FileWorkspaceScope, root: ActiveRoot, request: DownloadRequest): Promise<TransferStatus>;
  cancelTransfer(scope: FileWorkspaceScope, transferId: string): Promise<void>;
  subscribe(scope: FileWorkspaceScope, listener: (event: WorkspaceEvent) => void): Promise<() => void>;
}

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

export interface ActiveRoot {
  token: string;
  paneId: string;
  cwd: string;
  path: string;
  gitWorktree: boolean;
  revision: string;
}

export type FileKind = "file" | "directory" | "symlink";

export interface FileEntry {
  path: string;
  name: string;
  kind: FileKind;
  sizeBytes: string;
  modifiedMillis: string;
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
  overflowRecovery: boolean;
  nextPageToken?: string;
  complete: boolean;
}

export interface DirectoryWatchLease {
  snapshot: DirectoryListing;
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

export type WorkspaceEvent =
  | { kind: "rootChanged"; root: ActiveRoot }
  | { kind: "directoryChanged"; rootToken: string; directory: string; overflow: boolean }
  | { kind: "fileChanged"; rootToken: string; path: string; generation: string; operationId?: string }
  | { kind: "fileDeleted"; rootToken: string; path: string }
  | { kind: "transfer"; transfer: TransferStatus };

export interface FileWorkspaceClient {
  resolveActiveRoot(scope: FileWorkspaceScope): Promise<ActiveRoot>;
  listDirectory(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, pageToken?: string): Promise<DirectoryListing>;
  acquireDirectoryWatch(scope: FileWorkspaceScope, root: ActiveRoot, directory: string): Promise<DirectoryWatchLease>;
  openFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, signal?: AbortSignal): Promise<OpenFile>;
  writeText(scope: FileWorkspaceScope, root: ActiveRoot, request: WriteTextRequest): Promise<WriteTextResult>;
  mutate(scope: FileWorkspaceScope, root: ActiveRoot, mutation: FileMutation): Promise<void>;
  startDownload(scope: FileWorkspaceScope, root: ActiveRoot, request: DownloadRequest): Promise<TransferStatus>;
  cancelTransfer(scope: FileWorkspaceScope, transferId: string): Promise<void>;
  subscribe(scope: FileWorkspaceScope, listener: (event: WorkspaceEvent) => void): Promise<() => void>;
}

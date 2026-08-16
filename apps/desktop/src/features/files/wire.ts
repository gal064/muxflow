import type { ActiveRoot, DirectoryListing, DownloadRequest, FileEntry, FileWorkspaceScope, TextFile, TransferStatus } from "./types";
import {
  isTerminalTransferState,
  transferCleanupStatusFromWire,
  transferFailureKindFromWire,
  transferOutcomeFromWire,
  transferStateFromWire,
  validateTransferStateOutcome,
} from "../transfers/transferState";

export interface WireMetadata {
  path: string; name: string; kind: string; size: string; modifiedUnixMillis: string; mode: number;
  symlink: boolean; symlinkTarget: string; expandable: boolean; generation: string; mime: string; imagePreviewEligible: boolean;
  symlinkTargetKind?: string;
}
export interface WireRoot { paneId: string; root: string; rootToken: string; gitWorktree: boolean; serverIdentity: string; topologyGeneration: string; rootGeneration: string }
export interface WireDirectory { watchId: string; root: string; path: string; generation: string; entries: WireMetadata[]; overflowed: boolean; authoritative: boolean; nextPageToken: string; complete: boolean; recoveredFromOverflow?: boolean }
export interface WireContent { metadata?: WireMetadata; kind: "text" | "binary" | "image" | "tooLarge" | "unspecified"; content: number[]; generation: string }
export interface WireResponse { operationId: string; activeRoot?: WireRoot; directory?: WireDirectory; content?: WireContent; metadata?: WireMetadata; rootUnchanged?: boolean }
export interface WireFileEvent { operationId: string; activeRoot?: WireRoot; directory?: WireDirectory; metadata?: WireMetadata; deleted?: boolean; rootToken?: string; watchId?: string; transferId: string; transferredBytes: string; totalBytes: string; state: string; error: string }
export interface WireDownloadEvent {
  transferId: string; state: string; transferredBytes?: string; totalBytes?: string; totalKnown?: boolean;
  throughputBytesPerSecond?: string | number; etaSeconds?: number; destination?: string;
  artifactKind?: "file" | "tarArchive"; blake3?: string; error?: string; cleanupError?: string;
  cleanupStatus?: string; outcome?: string; failureKind?: string; serverIdentity?: string;
  expectedServerIdentity?: string; connectionEpoch?: string; terminal?: boolean;
}
export interface WireFileIoEvent {
  transferId: string; operationId?: string; state: string;
  metadata?: WireMetadata; metadataOnly?: boolean; contentKind?: WireContent["kind"];
  transferredBytes?: string; totalBytes?: string; generation?: string; blake3?: string; error?: string;
}

export function mapRoot(value: WireRoot): ActiveRoot {
  return { token: value.rootToken, paneId: value.paneId, cwd: value.root, path: value.root, gitWorktree: value.gitWorktree, revision: String(value.rootGeneration) };
}

export function mapDirectory(value: WireDirectory, rootToken: string): DirectoryListing {
  return {
    rootToken, directory: value.path, revision: String(value.generation), entries: value.entries.map(mapEntry),
    recoveredFromOverflow: Boolean(value.recoveredFromOverflow),
    nextPageToken: value.nextPageToken || undefined, complete: value.complete,
  };
}

export function mapEntry(value: WireMetadata): FileEntry {
  return {
    path: value.path, name: value.name, kind: value.kind === "directory" || value.kind === "symlink" ? value.kind : "file",
    sizeBytes: String(value.size), modifiedMillis: String(value.modifiedUnixMillis),
    generation: String(value.generation), executable: (value.mode & 0o111) !== 0,
    ...(value.symlinkTarget ? { symlinkTarget: value.symlinkTarget } : {}), expandable: value.expandable,
    ...(value.symlinkTargetKind && ["file", "directory", "missing", "other"].includes(value.symlinkTargetKind)
      ? { targetKind: value.symlinkTargetKind as NonNullable<FileEntry["targetKind"]> } : {}),
  };
}

/**
 * Whether a precise change event described an entry the tree can actually
 * draw. A deleted or unreadable path arrives with a default-shaped metadata
 * record, and patching a listing from that would invent a zero-byte row.
 */
export function isRenderableEntry(value: WireMetadata | undefined): value is WireMetadata {
  return Boolean(value && value.path && value.name && value.kind && value.kind !== "unspecified");
}

export function detectLineEnding(value: string): TextFile["lineEnding"] {
  const crlf = (value.match(/\r\n/g) ?? []).length;
  const lf = (value.match(/(?<!\r)\n/g) ?? []).length;
  if (crlf && lf) return "mixed";
  if (crlf) return "crlf";
  if (lf) return "lf";
  return "none";
}

export function applyLineEnding(value: string, lineEnding: TextFile["lineEnding"]): string {
  if (lineEnding !== "crlf") return value;
  return value.replace(/\r?\n/g, "\r\n");
}

export function mapDownloadEvent(
  event: WireDownloadEvent,
  request: DownloadRequest,
  scope: FileWorkspaceScope,
  scopeKey: string,
): TransferStatus {
  if (event.serverIdentity !== scope.serverIdentity || event.expectedServerIdentity !== scope.serverIdentity || event.connectionEpoch !== String(scope.terminalEpoch)) {
    throw new Error("Host returned a download event for a stale connection scope.");
  }
  const state = transferStateFromWire(event.state);
  if (event.terminal !== isTerminalTransferState(state)) {
    throw new Error("Host returned an inconsistent download terminal marker.");
  }
  const outcome = transferOutcomeFromWire(event.outcome, state);
  const failureKind = transferFailureKindFromWire(event.failureKind);
  const cleanupStatus = transferCleanupStatusFromWire(event.cleanupStatus);
  validateTransferStateOutcome(state, outcome, failureKind);
  const completedBytes = checkedDecimal(event.transferredBytes ?? "0", "downloaded byte count");
  const totalBytes = event.totalBytes === undefined ? undefined : checkedDecimal(event.totalBytes, "download total");
  const bytesPerSecond = event.throughputBytesPerSecond === undefined ? undefined
    : typeof event.throughputBytesPerSecond === "number"
      ? Number.isFinite(event.throughputBytesPerSecond) && event.throughputBytesPerSecond >= 0
        ? String(Math.floor(event.throughputBytesPerSecond)) : undefined
      : checkedDecimal(event.throughputBytesPerSecond, "download throughput");
  return {
    id: event.transferId, scopeKey, path: request.path, destination: event.destination ?? request.destination, kind: request.kind, state,
    ...(outcome ? { outcome } : {}),
    ...(failureKind ? { failureKind } : {}),
    completedBytes, ...(totalBytes !== undefined && event.totalKnown !== false ? { totalBytes } : {}),
    filesCompleted: state === "completed" ? "1" : "0", ...(bytesPerSecond !== undefined ? { bytesPerSecond } : {}),
    ...(event.etaSeconds !== undefined && Number.isFinite(event.etaSeconds) ? { etaSeconds: event.etaSeconds } : {}),
    ...(event.blake3 ? { digest: event.blake3 } : {}), ...(event.error ? { error: event.error } : {}),
    ...(event.cleanupError ? { cleanupError: event.cleanupError } : {}),
    ...(cleanupStatus ? { cleanupStatus } : {}),
  };
}

export function checkedDecimal(value: string | number, label: string): string {
  const text = String(value);
  if (!/^(0|[1-9]\d*)$/.test(text)) throw new Error(`Host returned an invalid ${label}.`);
  return text;
}

import { Channel, invoke } from "@tauri-apps/api/core";
import { IMAGE_PREVIEW_LIMIT_BYTES, TEXT_FILE_LIMIT_BYTES } from "./types";
import type {
  ActiveRoot,
  BinaryFile,
  DirectoryListing,
  DirectoryWatchLease,
  DownloadRequest,
  FileEntry,
  FileMutation,
  FileWorkspaceClient,
  FileWorkspaceScope,
  OpenFile,
  TextFile,
  TransferStatus,
  WorkspaceEvent,
  WriteTextRequest,
  WriteTextResult,
} from "./types";
import {
  isTerminalTransferState,
  transferCleanupStatusFromWire,
  transferFailureKindFromWire,
  transferOutcomeFromWire,
  transferStateFromWire,
  validateTransferStateOutcome,
} from "../transfers/transferState";

interface WireMetadata {
  path: string; name: string; kind: string; size: string; modifiedUnixMillis: string; mode: number;
  symlink: boolean; symlinkTarget: string; expandable: boolean; generation: string; mime: string; imagePreviewEligible: boolean;
  symlinkTargetKind?: string;
}
interface WireRoot { paneId: string; root: string; rootToken: string; gitWorktree: boolean; serverIdentity: string; topologyGeneration: string; rootGeneration: string }
interface WireDirectory { watchId: string; root: string; path: string; generation: string; entries: WireMetadata[]; overflowed: boolean; authoritative: boolean; nextPageToken: string; complete: boolean }
interface WireContent { metadata?: WireMetadata; kind: "text" | "binary" | "image" | "tooLarge" | "unspecified"; content: number[]; generation: string }
interface WireResponse { operationId: string; activeRoot?: WireRoot; directory?: WireDirectory; content?: WireContent; metadata?: WireMetadata }
export interface WireFileEvent { operationId: string; activeRoot?: WireRoot; directory?: WireDirectory; metadata?: WireMetadata; deleted?: boolean; rootToken?: string; watchId?: string; transferId: string; transferredBytes: string; totalBytes: string; state: string; error: string }
interface WireDownloadEvent {
  transferId: string; state: string; transferredBytes?: string; totalBytes?: string; totalKnown?: boolean;
  throughputBytesPerSecond?: string | number; etaSeconds?: number; destination?: string;
  artifactKind?: "file" | "tarArchive"; blake3?: string; error?: string; cleanupError?: string;
  cleanupStatus?: string; outcome?: string; failureKind?: string; serverIdentity?: string;
  expectedServerIdentity?: string; connectionEpoch?: string; terminal?: boolean;
}
interface WireFileIoEvent {
  transferId: string; operationId?: string; state: string; purpose?: "text" | "imagePreview";
  metadata?: WireMetadata; metadataOnly?: boolean; contentKind?: WireContent["kind"];
  transferredBytes?: string; totalBytes?: string; generation?: string; blake3?: string; error?: string;
}
interface FileReadTransfer { metadata: WireMetadata; contentKind: WireContent["kind"]; bytes?: Uint8Array; generation: string }

const encoder = new TextEncoder();
const fatalDecoder = new TextDecoder("utf-8", { fatal: true });

/** Renderer adapter for the production host file service. */
export class TauriFileWorkspaceClient implements FileWorkspaceClient {
  readonly #listeners = new Set<(event: WorkspaceEvent) => void>();
  readonly #rootTokens = new Map<string, string>();
  readonly #watches = new Map<string, { clientId: string; count: number; watchId: string; ready: Promise<DirectoryListing> }>();

  async resolveActiveRoot(scope: FileWorkspaceScope): Promise<ActiveRoot> {
    const response = await this.#request(scope, {
      operation: "resolveActiveRoot", operationId: crypto.randomUUID(), paneId: scope.paneId,
      expectedServerIdentity: scope.serverIdentity, expectedTopologyGeneration: String(scope.generation),
    });
    if (!response.activeRoot) throw new Error("Host omitted the active root.");
    return this.#root(response.activeRoot);
  }

  async listDirectory(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, pageToken = ""): Promise<DirectoryListing> {
    const response = await this.#request(scope, this.#rootCommand(root, {
      operation: "listDirectory", operationId: crypto.randomUUID(), path: directory, pageToken, pageSize: 4096,
    }));
    if (!response.directory) throw new Error("Host omitted the directory listing.");
    return this.#directory(response.directory, root.token);
  }

  async acquireDirectoryWatch(scope: FileWorkspaceScope, root: ActiveRoot, directory: string): Promise<DirectoryWatchLease> {
    const key = watchKey(scope, root, directory);
    let record = this.#watches.get(key);
    if (record) record.count += 1;
    else {
      const watchId = crypto.randomUUID();
      const ready = this.#request(scope, this.#rootCommand(root, {
        operation: "watchDirectory", operationId: crypto.randomUUID(), path: directory, watchId,
      })).then((response) => {
        if (!response.directory) throw new Error("Host omitted the watch bootstrap snapshot.");
        return this.#directory(response.directory, root.token);
      });
      record = { clientId: scope.clientId, count: 1, watchId, ready };
      this.#watches.set(key, record);
      try { await ready; } catch (error) { if (this.#watches.get(key) === record) this.#watches.delete(key); throw error; }
    }
    const snapshot = await record.ready;
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      const current = this.#watches.get(key);
      if (!current) return;
      current.count -= 1;
      if (current.count > 0) return;
      this.#watches.delete(key);
      void current.ready.then(() => this.#request(
        { ...scope, clientId: current.clientId },
        { operation: "unwatchDirectory", operationId: crypto.randomUUID(), watchId: current.watchId },
      )).catch(() => undefined);
    };
    return { snapshot, release };
  }

  async openFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, signal?: AbortSignal): Promise<OpenFile> {
    let transfer = await this.#readFile(scope, root, path, "text", signal);
    if (transfer.contentKind === "text" && transfer.bytes) {
      let value: string;
      try { value = fatalDecoder.decode(transfer.bytes); } catch { throw new Error("Host returned invalid UTF-8 for a text file."); }
      const file: TextFile = {
        path: transfer.metadata.path,
        content: value,
        generation: transfer.generation,
        sizeBytes: String(transfer.metadata.size),
        lineEnding: detectLineEnding(value),
        encoding: "utf-8",
      };
      return { kind: "text", file };
    }
    if (transfer.contentKind === "image" && transfer.metadata.imagePreviewEligible) {
      transfer = await this.#readFile(scope, root, path, "imagePreview", signal);
    }
    const file: BinaryFile = {
      path: transfer.metadata.path,
      generation: transfer.generation,
      sizeBytes: String(transfer.metadata.size),
      mime: transfer.metadata.mime || "application/octet-stream",
      previewKind: transfer.contentKind === "image" ? "image" : "binary",
      ...(transfer.bytes ? { previewBytes: transfer.bytes } : {}),
    };
    return { kind: "binary", file };
  }

  async writeText(scope: FileWorkspaceScope, root: ActiveRoot, request: WriteTextRequest): Promise<WriteTextResult> {
    const normalized = applyLineEnding(request.content, request.lineEnding);
    const content = encoder.encode(normalized);
    if (content.byteLength > TEXT_FILE_LIMIT_BYTES) throw new Error("Text files larger than 10 MiB cannot be edited.");
    const completed = await this.#fileIo("start_file_write", {
      clientId: scope.clientId, profileId: scope.hostProfileId, expectedServerIdentity: scope.serverIdentity,
      connectionEpoch: String(scope.terminalEpoch), root: root.path, rootToken: root.token, path: request.path,
      operationId: request.operationId, fileGeneration: request.baseGeneration, content: [...content],
    });
    if (!completed.metadata) throw new Error("Host omitted saved file metadata.");
    return {
      path: completed.metadata.path, generation: String(completed.generation || completed.metadata.generation),
      operationId: completed.operationId ?? request.operationId, sizeBytes: String(completed.metadata.size),
    };
  }

  async mutate(scope: FileWorkspaceScope, root: ActiveRoot, mutation: FileMutation): Promise<void> {
    const command: Record<string, unknown> = this.#rootCommand(root, { operation: "mutate", operationId: crypto.randomUUID() });
    if (mutation.kind === "createFile" || mutation.kind === "createDirectory") Object.assign(command, {
      mutation: "create", path: joinPath(mutation.parent, mutation.name), createDirectory: mutation.kind === "createDirectory",
    });
    else if (mutation.kind === "delete") Object.assign(command, { mutation: "delete", path: mutation.path, nonEmptyConfirmed: mutation.confirmedNonEmpty });
    else Object.assign(command, { mutation: mutation.kind, path: mutation.path, destination: mutation.destination, overwriteConfirmed: mutation.overwrite, nonEmptyConfirmed: mutation.confirmedNonEmpty });
    await this.#request(scope, command);
  }

  async startDownload(scope: FileWorkspaceScope, root: ActiveRoot, request: DownloadRequest): Promise<TransferStatus> {
    if (!request.destination) throw new Error("Choose a local download destination.");
    let latest: TransferStatus | undefined;
    const onEvent = new Channel<WireDownloadEvent>();
    onEvent.onmessage = (event) => {
      let transfer: TransferStatus;
      try {
        transfer = mapDownloadEvent(event, request, scope);
      } catch (error) {
        transfer = {
          id: event.transferId || crypto.randomUUID(), scopeKey: keyForTransferConnection(scope), path: request.path, destination: request.destination, kind: request.kind,
          state: "failed", outcome: "notPublished", failureKind: "transfer", completedBytes: "0", filesCompleted: "0",
          error: error instanceof Error ? error.message : String(error),
        };
      }
      latest = transfer;
      this.#publish({ kind: "transfer", transfer });
    };
    const transferId = await invoke<string>("start_download", {
      clientId: scope.clientId,
      profileId: scope.hostProfileId,
      expectedServerIdentity: scope.serverIdentity,
      connectionEpoch: String(scope.terminalEpoch),
      root: root.path,
      rootToken: root.token,
      source: request.path,
      destination: request.destination,
      folder: request.kind === "folder",
      collision: request.collision === "overwrite" ? "overwriteConfirmed" : request.collision,
      onEvent,
    });
    return latest ?? {
      id: transferId, scopeKey: keyForTransferConnection(scope), path: request.path, destination: request.destination, kind: request.kind,
      state: "queued", completedBytes: "0", filesCompleted: "0",
    };
  }

  cancelTransfer(_scope: FileWorkspaceScope, transferId: string): Promise<void> {
    return invoke("cancel_download", { transferId });
  }

  async subscribe(_scope: FileWorkspaceScope, listener: (event: WorkspaceEvent) => void): Promise<() => void> {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Called only after the terminal event sequencer admitted this host frame. */
  publishWireEvent(event: WireFileEvent): void {
    if (event.activeRoot) this.#publish({ kind: "rootChanged", root: this.#root(event.activeRoot) });
    if (event.directory) {
      if (event.rootToken) this.#publish({ kind: "directoryChanged", rootToken: event.rootToken, directory: event.directory.path, overflow: event.directory.overflowed });
    }
    if (event.metadata && event.rootToken) {
      if (event.deleted) this.#publish({ kind: "fileDeleted", rootToken: event.rootToken, path: event.metadata.path });
      else this.#publish({ kind: "fileChanged", rootToken: event.rootToken, path: event.metadata.path, generation: String(event.metadata.generation), ...(event.operationId ? { operationId: event.operationId } : {}) });
    }
  }

  #publish(event: WorkspaceEvent): void { for (const listener of this.#listeners) listener(event); }

  async #readFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, purpose: "text" | "imagePreview", signal?: AbortSignal): Promise<FileReadTransfer> {
    const chunks: Uint8Array[] = [];
    let expectedOffset = 0n;
    const completed = await this.#fileIo("start_file_read", {
      clientId: scope.clientId, profileId: scope.hostProfileId, expectedServerIdentity: scope.serverIdentity,
      connectionEpoch: String(scope.terminalEpoch), root: root.path, rootToken: root.token, path, purpose,
    }, (offset, chunk) => {
      if (offset !== expectedOffset) throw new Error("Bulk file chunks arrived out of sequence.");
      expectedOffset += BigInt(chunk.byteLength);
      const limit = purpose === "text" ? TEXT_FILE_LIMIT_BYTES : IMAGE_PREVIEW_LIMIT_BYTES;
      if (expectedOffset > BigInt(limit)) throw new Error(`Bulk file content exceeded the ${purpose === "text" ? "10 MiB" : "25 MiB"} limit.`);
      chunks.push(chunk);
    }, signal);
    if (!completed.metadata || !completed.contentKind) throw new Error("Host omitted file content metadata.");
    const total = completed.totalBytes === undefined ? expectedOffset : BigInt(completed.totalBytes);
    if (!completed.metadataOnly && total !== expectedOffset) throw new Error("Bulk file byte count verification failed.");
    const bytes = completed.metadataOnly ? undefined : concatenate(chunks, Number(expectedOffset));
    return {
      metadata: completed.metadata, contentKind: completed.contentKind,
      generation: String(completed.generation || completed.metadata.generation), ...(bytes ? { bytes } : {}),
    };
  }

  async #fileIo(
    command: "start_file_read" | "start_file_write",
    args: Record<string, unknown>,
    onChunk?: (offset: bigint, chunk: Uint8Array) => void,
    signal?: AbortSignal,
  ): Promise<WireFileIoEvent> {
    return new Promise((resolve, reject) => {
      let settled = false;
      let metadata: WireMetadata | undefined;
      let contentKind: WireContent["kind"] | undefined;
      let transferId: string | undefined;
      const abort = () => {
        if (settled) return;
        settled = true;
        if (transferId) void invoke("cancel_file_io", { transferId }).catch(() => undefined);
        reject(new DOMException("File load was cancelled.", "AbortError"));
      };
      signal?.addEventListener("abort", abort, { once: true });
      const finishError = (error: unknown) => {
        if (settled) return;
        settled = true;
        signal?.removeEventListener("abort", abort);
        reject(error instanceof Error ? error : new Error(String(error)));
      };
      const channel = new Channel<ArrayBuffer>();
      channel.onmessage = (raw) => {
        if (settled) return;
        try {
          const frame = raw instanceof ArrayBuffer ? new Uint8Array(raw) : new Uint8Array(raw as unknown as ArrayBuffer);
          if (frame.byteLength < 1) throw new Error("Host emitted an empty file-I/O frame.");
          if (frame[0] === 2) {
            if (frame.byteLength < 9 || !onChunk) throw new Error("Host emitted an invalid file chunk.");
            const view = new DataView(frame.buffer, frame.byteOffset + 1, 8);
            const offset = view.getBigUint64(0, false);
            onChunk(offset, frame.slice(9));
            return;
          }
          if (frame[0] < 1 || frame[0] > 4) throw new Error("Host emitted an unknown file-I/O frame.");
          const event = JSON.parse(new TextDecoder().decode(frame.subarray(1))) as WireFileIoEvent;
          metadata = event.metadata ?? metadata;
          contentKind = event.contentKind ?? contentKind;
          if (frame[0] === 4 || event.state === "error" || event.state === "cancelled") {
            finishError(new Error(event.error || (event.state === "cancelled" ? "File transfer cancelled." : "File transfer failed.")));
          } else if (frame[0] === 3) {
            settled = true;
            signal?.removeEventListener("abort", abort);
            resolve({ ...event, ...(metadata ? { metadata } : {}), ...(contentKind ? { contentKind } : {}) });
          }
        } catch (error) { finishError(error); }
      };
      if (signal?.aborted) { abort(); return; }
      invoke<string>(command, { ...args, onEvent: channel }).then((id) => {
        transferId = id;
        if (signal?.aborted) void invoke("cancel_file_io", { transferId: id }).catch(() => undefined);
      }).catch(finishError);
    });
  }

  async #request(scope: FileWorkspaceScope, command: Record<string, unknown>): Promise<WireResponse> {
    return invoke("file_request", { clientId: scope.clientId, command });
  }

  #rootCommand(root: ActiveRoot, command: Record<string, unknown>): Record<string, unknown> {
    return { ...command, root: root.path, rootToken: root.token };
  }

  #root(value: WireRoot): ActiveRoot {
    this.#rootTokens.set(value.root, value.rootToken);
    return { token: value.rootToken, paneId: value.paneId, cwd: value.root, path: value.root, gitWorktree: value.gitWorktree, revision: String(value.rootGeneration) };
  }

  #directory(value: WireDirectory, rootToken: string): DirectoryListing {
    this.#rootTokens.set(value.root, rootToken);
    return {
      rootToken, directory: value.path, revision: String(value.generation), entries: value.entries.map(mapEntry),
      overflowRecovery: value.overflowed, nextPageToken: value.nextPageToken || undefined, complete: value.complete,
    };
  }

}

export function keyForScope(scope: FileWorkspaceScope): string {
  return [scope.clientId, scope.hostProfileId, scope.serverIdentity, scope.terminalEpoch, scope.sessionId, scope.paneId].join("\0");
}

/** Downloads outlive Explorer pane/root/session selection within one live connection. */
export function keyForTransferConnection(scope: FileWorkspaceScope): string {
  return [scope.clientId, scope.serverIdentity, scope.terminalEpoch].join("\0");
}

export function sameRoot(left: ActiveRoot | undefined, right: ActiveRoot | undefined): boolean {
  return Boolean(left && right && left.token === right.token && left.path === right.path && left.paneId === right.paneId);
}

function watchKey(scope: FileWorkspaceScope, root: ActiveRoot, path: string): string { return `${scope.clientId}\0${root.token}\0${path}`; }
function joinPath(parent: string, name: string): string { return `${parent.replace(/\/$/, "")}/${name}`; }

function mapEntry(value: WireMetadata): FileEntry {
  return {
    path: value.path, name: value.name, kind: value.kind === "directory" || value.kind === "symlink" ? value.kind : "file",
    sizeBytes: String(value.size), modifiedMillis: String(value.modifiedUnixMillis), executable: (value.mode & 0o111) !== 0,
    ...(value.symlinkTarget ? { symlinkTarget: value.symlinkTarget } : {}), expandable: value.expandable,
    ...(value.symlinkTargetKind && ["file", "directory", "missing", "other"].includes(value.symlinkTargetKind)
      ? { targetKind: value.symlinkTargetKind as NonNullable<FileEntry["targetKind"]> } : {}),
  };
}

function detectLineEnding(value: string): TextFile["lineEnding"] {
  const crlf = (value.match(/\r\n/g) ?? []).length;
  const lf = (value.match(/(?<!\r)\n/g) ?? []).length;
  if (crlf && lf) return "mixed";
  if (crlf) return "crlf";
  if (lf) return "lf";
  return "none";
}

function applyLineEnding(value: string, lineEnding: TextFile["lineEnding"]): string {
  if (lineEnding !== "crlf") return value;
  return value.replace(/\r?\n/g, "\r\n");
}

function mapDownloadEvent(event: WireDownloadEvent, request: DownloadRequest, scope: FileWorkspaceScope): TransferStatus {
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
    id: event.transferId, scopeKey: keyForTransferConnection(scope), path: request.path, destination: event.destination ?? request.destination, kind: request.kind, state,
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

function checkedDecimal(value: string | number, label: string): string {
  const text = String(value);
  if (!/^(0|[1-9]\d*)$/.test(text)) throw new Error(`Host returned an invalid ${label}.`);
  return text;
}

function concatenate(chunks: Uint8Array[], total: number): Uint8Array {
  const value = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { value.set(chunk, offset); offset += chunk.byteLength; }
  return value;
}

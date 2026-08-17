import { Channel, invoke } from "@tauri-apps/api/core";
import { measurePerfOutcome, measurePerfRequest, recordPerfCounter, recordPerfHighWater, recordPerfJsonBytesDeferred, startPerfSpan } from "../../perf/probe";
import { IMAGE_PREVIEW_LIMIT_BYTES, TEXT_FILE_LIMIT_BYTES } from "./types";
import type {
  AcquireWatchOptions,
  ActiveRoot,
  BinaryFile,
  DirectoryListing,
  DirectoryWatchLease,
  DownloadRequest,
  FileMutation,
  FileWorkspaceClient,
  FileWorkspaceScope,
  ListDirectoryOptions,
  OpenFile,
  ResolveRootOptions,
  TextFile,
  TransferStatus,
  WorkspaceEvent,
  WriteTextRequest,
  WriteTextResult,
} from "./types";
import type { WireContent, WireDownloadEvent, WireFileEvent, WireFileIoEvent, WireMetadata, WireResponse } from "./wire";
import {
  applyLineEnding,
  detectLineEnding,
  isRenderableEntry,
  mapDirectory,
  mapDownloadEvent,
  mapEntry,
  mapRoot,
} from "./wire";

export type { WireFileEvent } from "./wire";

interface OpenedFile { metadata: WireMetadata; contentKind: WireContent["kind"]; bytes?: Uint8Array; generation: string }

/** One host watch, shared by every surface that asked for the same directory. */
interface WatchRecord {
  clientId: string;
  subscribers: number;
  watchId: string;
  ready: Promise<DirectoryListing>;
  /** Aborted only when the last subscriber leaves, never by one of them. */
  cancel: AbortController;
  /** Whether `ready` has settled, which is what makes a later join stale. */
  resolved: boolean;
}

const encoder = new TextEncoder();
const fatalDecoder = new TextDecoder("utf-8", { fatal: true });

/** Renderer adapter for the production host file service. */
export class TauriFileWorkspaceClient implements FileWorkspaceClient {
  readonly #listeners = new Set<(event: WorkspaceEvent) => void>();
  readonly #watches = new Map<string, WatchRecord>();

  async resolveActiveRoot(scope: FileWorkspaceScope, options: ResolveRootOptions = {}): Promise<ActiveRoot> {
    return this.#request(scope, {
      operation: "resolveActiveRoot", operationId: crypto.randomUUID(), paneId: scope.paneId,
      expectedServerIdentity: scope.serverIdentity, expectedTopologyGeneration: String(scope.generation),
      knownRootToken: options.knownRootToken ?? "",
    }, (response) => {
      if (!response.activeRoot) throw new Error("Host omitted the active root.");
      if (response.rootUnchanged) recordPerfCounter("explorer.rootProbeUnchanged");
      return mapRoot(response.activeRoot);
    }, "files.resolveActiveRoot");
  }

  async listDirectory(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, options: ListDirectoryOptions = {}): Promise<DirectoryListing> {
    recordPerfCounter("explorer.directoryListRequests");
    const operationId = crypto.randomUUID();
    return this.#request(scope, this.#rootCommand(root, {
      operation: "listDirectory", operationId, path: directory, pageToken: options.pageToken ?? "", pageSize: 4096,
    }), (response) => {
      if (!response.directory) throw new Error("Host omitted the directory listing.");
      recordPerfCounter("explorer.listPayloadEntries", response.directory.entries.length);
      recordPerfJsonBytesDeferred("explorer.listMappedPayloadBytes", response.directory);
      return mapDirectory(response.directory, root.token);
    }, "files.listDirectory.request", { operationId, signal: options.signal });
  }

  /**
   * Acquires a shared watch on one directory, whose bootstrap snapshot *is* the
   * directory's initial listing.
   *
   * Arming a watch already costs the host a full authoritative listing, so a
   * caller that also issued a list paid a second remote round trip for an
   * answer it was about to be handed. Callers take the lease and read
   * `snapshot` — but only when `fresh` says the snapshot is theirs. One host
   * watch serves every subscriber, and its bootstrap is produced once, so a
   * later joiner is handed a listing that may be arbitrarily old; treating that
   * as authoritative silently reverted rows and forced re-reads on a false
   * premise.
   *
   * Cancellation is per subscriber, never per watch. The remote request is only
   * abandoned when the last subscriber has gone: one caller aborting an
   * acquisition it no longer needs must not take another caller's watch — and
   * with it every event that caller depends on — down with it.
   */
  async acquireDirectoryWatch(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, options: AcquireWatchOptions = {}): Promise<DirectoryWatchLease> {
    recordPerfCounter("explorer.watchSubscribers");
    const key = watchKey(scope, root, directory);
    const existing = this.#watches.get(key);
    const held = existing ?? this.#armWatch(scope, root, directory, key);
    if (existing) existing.subscribers += 1;
    // Whether the bootstrap in flight (or already resolved) belongs to this
    // acquisition. A subscriber that joins before the answer lands shares the
    // request, so the answer is as new as its own would have been.
    const fresh = !held.resolved;

    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      recordPerfCounter("explorer.watchReleases");
      // The exact record this lease belongs to. A key alone would let a lease
      // from a retired watch decrement the refcount of the one that replaced it.
      if (this.#watches.get(key) !== held) return;
      held.subscribers -= 1;
      if (held.subscribers > 0) return;
      this.#watches.delete(key);
      this.#retireWatch(scope, held);
    };
    const abort = options.signal;
    if (abort?.aborted) {
      release();
      throw new DOMException("Directory watch was cancelled.", "AbortError");
    }
    // Abandoning a subscription answers this caller immediately. Waiting out
    // the shared request instead would hold a collapsed folder's expansion —
    // and the effect that owns it — until some *other* surface's watch landed.
    let abandon: ((reason: unknown) => void) | undefined;
    const abandoned = new Promise<never>((_, reject) => { abandon = reject; });
    const onAbort = () => {
      release();
      abandon?.(new DOMException("Directory watch was cancelled.", "AbortError"));
    };
    abort?.addEventListener("abort", onAbort, { once: true });
    try {
      const snapshot = await (abort ? Promise.race([held.ready, abandoned]) : held.ready);
      return { snapshot, fresh, release };
    } catch (error) {
      release();
      throw error;
    } finally {
      abort?.removeEventListener("abort", onAbort);
    }
  }

  /** Arms one host watch, shared by every subscriber that asks for it. */
  #armWatch(scope: FileWorkspaceScope, root: ActiveRoot, directory: string, key: string): WatchRecord {
    recordPerfCounter("explorer.watchRequests");
    const watchId = crypto.randomUUID();
    const operationId = crypto.randomUUID();
    const cancel = new AbortController();
    const ready = this.#request(scope, this.#rootCommand(root, {
      operation: "watchDirectory", operationId, path: directory, watchId,
    }), (response) => {
      if (!response.directory) throw new Error("Host omitted the watch bootstrap snapshot.");
      recordPerfCounter("explorer.watchBootstrapEntries", response.directory.entries.length);
      return mapDirectory(response.directory, root.token);
    }, "files.watchDirectory.request", { operationId, signal: cancel.signal });
    const record: WatchRecord = {
      clientId: scope.clientId, subscribers: 1, watchId, ready, cancel, resolved: false,
    };
    // Marked before any subscriber's own continuation runs, so "did this
    // acquisition produce the snapshot?" is decided by arrival order rather
    // than by which promise callback happened to be scheduled first. A failed
    // record is deliberately *not* dropped here: every subscriber releases its
    // lease on the way out, and the last release is what gives the watch back
    // to the host. Dropping the record first made that release a no-op and
    // orphaned the registration.
    const settled = () => { record.resolved = true; };
    void ready.then(settled, settled);
    this.#watches.set(key, record);
    recordPerfHighWater("explorer.activeWatches", this.#watches.size);
    return record;
  }

  /**
   * Gives one watch back to the host, whatever happened to its bootstrap.
   *
   * The unwatch is sent even when the bootstrap failed. The watch ID is minted
   * here, and the host registers the watch before it lists — so a control-lane
   * timeout, which is exactly what a large directory on a slow link produces,
   * leaves a registration the desktop has stopped counting. Against a 128-watch
   * budget those orphans end as "every expansion is refused". Cancelling first
   * keeps the common case cheap: a watch abandoned before it was ever armed
   * costs the host nothing.
   */
  #retireWatch(scope: FileWorkspaceScope, record: WatchRecord): void {
    record.cancel.abort();
    recordPerfCounter("explorer.unwatchRequests");
    const unwatch = () => this.#request(
      { ...scope, clientId: record.clientId },
      { operation: "unwatchDirectory", operationId: crypto.randomUUID(), watchId: record.watchId },
      () => undefined,
      "files.unwatchDirectory.request",
    ).catch(() => undefined);
    void record.ready.then(unwatch, unwatch);
  }

  /**
   * Forgets every shared watch belonging to a connection that has gone.
   *
   * This client outlives any one bridge — it is built once for the app — so
   * without this the records for a dead connection stay in the map forever,
   * holding promises nothing can settle and a refcount nothing can release.
   * The host has already lost the whole connection, so there is no unwatch to
   * send: the registrations went with it.
   */
  retireConnection(clientId: string): void {
    for (const [key, record] of [...this.#watches]) {
      if (record.clientId !== clientId) continue;
      this.#watches.delete(key);
      record.cancel.abort();
    }
  }

  /**
   * Instrumented because "opening a file is slow" was a report nothing in the
   * app could confirm or refute: every perf span belonged to the terminal, and
   * the file lane — the one spawning an ssh process per open — had none.
   */
  openFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, signal?: AbortSignal): Promise<OpenFile> {
    recordPerfCounter("file.openRequests");
    return measurePerfOutcome("file.open", () => this.#openFile(scope, root, path, signal));
  }

  async #openFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, signal?: AbortSignal): Promise<OpenFile> {
    const opened = await this.#readOpenedFile(scope, root, path, signal);
    if (opened.contentKind === "text" && opened.bytes) {
      let value: string;
      try { value = fatalDecoder.decode(opened.bytes); } catch { throw new Error("Host returned invalid UTF-8 for a text file."); }
      const file: TextFile = {
        path: opened.metadata.path,
        content: value,
        generation: opened.generation,
        sizeBytes: String(opened.metadata.size),
        lineEnding: detectLineEnding(value),
        encoding: "utf-8",
      };
      return { kind: "text", file };
    }
    const file: BinaryFile = {
      path: opened.metadata.path,
      generation: opened.generation,
      sizeBytes: String(opened.metadata.size),
      mime: opened.metadata.mime || "application/octet-stream",
      previewKind: opened.contentKind === "image" ? "image" : "binary",
      ...(opened.bytes ? { previewBytes: opened.bytes } : {}),
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
    await this.#request(scope, command, () => undefined, "files.mutationAck");
  }

  async startDownload(scope: FileWorkspaceScope, root: ActiveRoot, request: DownloadRequest): Promise<TransferStatus> {
    if (!request.destination) throw new Error("Choose a local download destination.");
    const scopeKey = keyForTransferConnection(scope);
    let latest: TransferStatus | undefined;
    const onEvent = new Channel<WireDownloadEvent>();
    onEvent.onmessage = (event) => {
      let transfer: TransferStatus;
      try {
        transfer = mapDownloadEvent(event, request, scope, scopeKey);
      } catch (error) {
        transfer = {
          id: event.transferId || crypto.randomUUID(), scopeKey, path: request.path, destination: request.destination, kind: request.kind,
          state: "failed", outcome: "notPublished", failureKind: "transfer", completedBytes: "0", filesCompleted: "0",
          error: error instanceof Error ? error.message : String(error),
        };
      }
      latest = transfer;
      this.#publish({ kind: "transfer", transfer });
    };
    const downloadCommand = {
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
    };
    const boundary = { ...downloadCommand, onEvent };
    const transferId = await measurePerfRequest("file.downloadAdmission", "file", boundary, async (requestBoundary) => {
      const id = await invoke<string>("start_download", requestBoundary);
      if (!id) throw new Error("Native download admission omitted its transfer ID.");
      return id;
    });
    return latest ?? {
      id: transferId, scopeKey, path: request.path, destination: request.destination, kind: request.kind,
      state: "queued", completedBytes: "0", filesCompleted: "0",
    };
  }

  cancelTransfer(_scope: FileWorkspaceScope, transferId: string): Promise<void> {
    const boundary = { transferId };
    return measurePerfRequest(
      "file.downloadCancellation", "file", boundary, (request) => invoke("cancel_download", request),
    );
  }

  async subscribe(_scope: FileWorkspaceScope, listener: (event: WorkspaceEvent) => void): Promise<() => void> {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /**
   * Called only after the terminal event sequencer admitted this host frame.
   *
   * Everything the host mapped is carried through: an authoritative rescan
   * arrives as the listing it is, and a precise change arrives with the exact
   * entry it describes. Consumers decide whether to patch, replace, or recover.
   */
  publishWireEvent(event: WireFileEvent): void {
    if (event.activeRoot) this.#publish({ kind: "rootChanged", root: mapRoot(event.activeRoot) });
    if (event.directory && event.rootToken) {
      this.#publish({
        kind: "directorySnapshot",
        rootToken: event.rootToken,
        listing: mapDirectory(event.directory, event.rootToken),
      });
    }
    if (event.metadata && event.rootToken) {
      if (event.deleted) this.#publish({ kind: "fileDeleted", rootToken: event.rootToken, path: event.metadata.path });
      else this.#publish({
        kind: "fileChanged",
        rootToken: event.rootToken,
        path: event.metadata.path,
        generation: String(event.metadata.generation),
        ...(event.operationId ? { operationId: event.operationId } : {}),
        ...(isRenderableEntry(event.metadata) ? { entry: mapEntry(event.metadata) } : {}),
      });
    }
  }

  #publish(event: WorkspaceEvent): void { for (const listener of this.#listeners) listener(event); }

  /** One bulk request, one classification, one continuous body. */
  async #readOpenedFile(scope: FileWorkspaceScope, root: ActiveRoot, path: string, signal?: AbortSignal): Promise<OpenedFile> {
    const chunks: Uint8Array[] = [];
    let expectedOffset = 0n;
    const firstContent = startPerfSpan("file.timeToFirstContent");
    let sawContent = false;
    const completed = await this.#fileIo("start_file_read", {
      clientId: scope.clientId, profileId: scope.hostProfileId, expectedServerIdentity: scope.serverIdentity,
      connectionEpoch: String(scope.terminalEpoch), root: root.path, rootToken: root.token, path,
    }, (offset, chunk) => {
      if (!sawContent) {
        sawContent = true;
        firstContent();
      }
      recordPerfCounter("file.contentChunks");
      recordPerfCounter("file.contentBytes", chunk.byteLength);
      if (offset !== expectedOffset) throw new Error("Bulk file chunks arrived out of sequence.");
      expectedOffset += BigInt(chunk.byteLength);
      if (expectedOffset > BigInt(IMAGE_PREVIEW_LIMIT_BYTES)) throw new Error("Bulk file content exceeded the 25 MiB limit.");
      chunks.push(chunk);
    }, signal);
    if (!sawContent) firstContent();
    if (!completed.metadata || !completed.contentKind) throw new Error("Host omitted file content metadata.");
    const total = completed.totalBytes === undefined ? expectedOffset : BigInt(completed.totalBytes);
    if (!completed.metadataOnly && total !== expectedOffset) throw new Error("Bulk file byte count verification failed.");
    const limit = completed.contentKind === "text" ? TEXT_FILE_LIMIT_BYTES : IMAGE_PREVIEW_LIMIT_BYTES;
    if (expectedOffset > BigInt(limit)) throw new Error(`Bulk file content exceeded the ${completed.contentKind === "text" ? "10 MiB" : "25 MiB"} limit.`);
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
    recordPerfCounter("file.ioRequestAttempts");
    return new Promise((resolve, reject) => {
      let settled = false;
      let metadata: WireMetadata | undefined;
      let contentKind: WireContent["kind"] | undefined;
      let transferId: string | undefined;
      const abort = () => {
        if (settled) return;
        settled = true;
        recordPerfCounter("file.ioRequestCancellations");
        if (transferId) cancelFileIo(transferId);
        reject(new DOMException("File load was cancelled.", "AbortError"));
      };
      signal?.addEventListener("abort", abort, { once: true });
      const finishError = (error: unknown, cancelled = false) => {
        if (settled) return;
        settled = true;
        recordPerfCounter(cancelled ? "file.ioRequestCancellations" : "file.ioRequestFailures");
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
            finishError(
              new Error(event.error || (event.state === "cancelled" ? "File transfer cancelled." : "File transfer failed.")),
              event.state === "cancelled",
            );
          } else if (frame[0] === 3) {
            settled = true;
            signal?.removeEventListener("abort", abort);
            recordPerfCounter("file.ioRequestSuccesses");
            resolve({ ...event, ...(metadata ? { metadata } : {}), ...(contentKind ? { contentKind } : {}) });
          }
        } catch (error) { finishError(error); }
      };
      if (signal?.aborted) { abort(); return; }
      const boundary = { ...args, onEvent: channel };
      measurePerfRequest("file.ioAdmission", "file", boundary, async (requestBoundary) => {
        const id = await invoke<string>(command, requestBoundary);
        if (!id) throw new Error("Native file I/O admission omitted its transfer ID.");
        return id;
      }, { byteCounters: ["file.ioRequestBytes"] }).then((id) => {
        transferId = id;
        if (signal?.aborted) cancelFileIo(id);
      }).catch(finishError);
    });
  }

  /**
   * One control-lane file request.
   *
   * When the caller supplies an abort signal the renderer's operation ID is
   * carried to the host, so aborting stops bounded remote work rather than only
   * discarding its answer locally.
   */
  async #request<T>(
    scope: FileWorkspaceScope,
    command: Record<string, unknown>,
    validate: (response: WireResponse) => T,
    metricName: string,
    cancellable?: { operationId: string; signal?: AbortSignal },
  ): Promise<T> {
    const boundary = { clientId: scope.clientId, command };
    const abort = cancellable?.signal;
    if (abort?.aborted) throw new DOMException("Directory read was cancelled.", "AbortError");
    const operationId = cancellable?.operationId;
    const stopRemoteWork = () => {
      recordPerfCounter("explorer.listCancellations");
      void invoke("cancel_file_request", { clientId: scope.clientId, operationId }).catch(() => undefined);
    };
    if (abort) abort.addEventListener("abort", stopRemoteWork, { once: true });
    try {
      return await measurePerfRequest(metricName, "file", boundary, async (requestBoundary) => {
        const response = await invoke<WireResponse>("file_request", requestBoundary);
        return validate(response);
      });
    } finally {
      abort?.removeEventListener("abort", stopRemoteWork);
    }
  }

  #rootCommand(root: ActiveRoot, command: Record<string, unknown>): Record<string, unknown> {
    return { ...command, root: root.path, rootToken: root.token };
  }
}

function cancelFileIo(transferId: string): void {
  const boundary = { transferId };
  void measurePerfRequest(
    "file.ioCancellation", "file", boundary, (request) => invoke("cancel_file_io", request),
  ).catch(() => undefined);
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

function concatenate(chunks: Uint8Array[], total: number): Uint8Array {
  const value = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { value.set(chunk, offset); offset += chunk.byteLength; }
  return value;
}

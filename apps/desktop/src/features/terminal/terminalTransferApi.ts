import { Channel, invoke } from "@tauri-apps/api/core";
import {
  decimalBytes,
  validateLocalTerminalPathInspection,
  validateUploadPreflight,
  type TerminalTransferClient,
  type TerminalTransferProgress,
  type NativeTerminalClipboard,
  type TerminalTransferScope,
  type TerminalTransferState,
  type TransferCancelDisposition,
  type UploadCollisionPolicy,
  type UploadPreflight,
  type VerifiedTerminalUpload,
  validateClipboardDestinationName,
} from "./terminalTransfers";
import {
  isTerminalTransferState,
  transferCleanupStatusFromWire,
  transferFailureKindFromWire,
  transferOutcomeFromWire,
  transferStateFromWire,
  validateTransferStateOutcome,
} from "../transfers/transferState";

interface WirePreflight {
  sourcePath: string;
  name: string;
  sizeBytes: string;
  sourceKind?: string;
  readable?: boolean;
  destination?: string;
  collision?: boolean;
  collisionRenamed?: boolean;
  confirmationRequired?: boolean;
}

interface WireUploadEvent {
  transferId: string;
  sourcePath: string;
  name: string;
  state: string;
  transferredBytes?: string;
  totalBytes?: string;
  throughputBytesPerSecond?: string | number;
  etaSeconds?: number;
  destination?: string;
  blake3?: string;
  error?: string;
  cleanupError?: string;
  cleanupStatus?: string;
  outcome?: string;
  failureKind?: string;
  serverIdentity?: string;
  expectedServerIdentity?: string;
  connectionEpoch?: string;
  terminal?: boolean;
}

interface UploadOptions {
  collision: UploadCollisionPolicy;
  largeUploadConfirmed: boolean;
  imagePng: boolean;
}

interface WireCancelDisposition { disposition: string; phase: string }

const PREFLIGHT_DEADLINE_MS = 30_000;

export class TauriTerminalTransferClient implements TerminalTransferClient {
  async readNativeClipboard(): Promise<NativeTerminalClipboard | undefined> {
    const payload = await invoke<NativeTerminalClipboard | null>("read_native_terminal_clipboard");
    if (!payload) return undefined;
    if (payload.kind === "files") {
      if (!Array.isArray(payload.uris) || payload.uris.some((uri) => typeof uri !== "string")) {
        throw new Error("The native clipboard returned an invalid file list.");
      }
      return { kind: "files", uris: payload.uris };
    }
    if (payload.kind !== "image" || !payload.staged) {
      throw new Error("The native clipboard returned an unknown payload.");
    }
    decimalBytes(String(payload.staged.sizeBytes), "native staged PNG size");
    return {
      kind: "image",
      staged: {
        path: payload.staged.path,
        sizeBytes: String(payload.staged.sizeBytes),
        name: validateClipboardDestinationName(payload.staged.name),
      },
    };
  }

  async inspectLocalPaths(paths: readonly string[]) {
    const response = await invoke<Array<{ path: string; sizeBytes: string; name: string }>>("inspect_local_terminal_paths", {
      paths: [...paths],
    });
    if (response.length !== paths.length) {
      throw new Error("The local file inspection response did not preserve the requested file list.");
    }
    return response.map((item, index) => validateLocalTerminalPathInspection({
      path: item.path,
      sizeBytes: String(item.sizeBytes),
      name: item.name,
    }, paths[index]));
  }

  preflight(
    scope: TerminalTransferScope,
    sourcePath: string,
    destinationName: string,
    options: UploadOptions,
    onProgress?: (progress: TerminalTransferProgress) => void,
    signal?: AbortSignal,
  ): Promise<UploadPreflight> {
    return new Promise((resolve, reject) => {
      let settled = false;
      let preflightId: string | undefined;
      let cancelSent = false;
      const cancel = () => {
        if (!preflightId || cancelSent) return;
        cancelSent = true;
        void invoke("cancel_terminal_upload_preflight", { preflightId }).catch(() => undefined);
      };
      const finishError = (error: unknown) => {
        if (settled) return;
        settled = true;
        globalThis.clearTimeout(deadline);
        signal?.removeEventListener("abort", abort);
        reject(error instanceof Error ? error : new Error(String(error)));
      };
      const abort = () => {
        cancel();
        finishError(new DOMException("Terminal upload preflight was cancelled.", "AbortError"));
      };
      const deadline = globalThis.setTimeout(() => {
        cancel();
        finishError(new Error("Terminal upload preflight timed out after 30 seconds."));
      }, PREFLIGHT_DEADLINE_MS);
      signal?.addEventListener("abort", abort, { once: true });
      const onEvent = new Channel<WireUploadEvent & WirePreflight>();
      onEvent.onmessage = (event) => {
        if (settled) return;
        try {
          const progress = mapUploadEvent(event, scope);
          onProgress?.(progress);
          if (progress.state === "completed") {
            const response = validateUploadPreflight({
              sourcePath: event.sourcePath,
              name: destinationName,
              sizeBytes: String(event.sizeBytes),
              sourceKind: event.sourceKind === "directory" ? "directory" : event.sourceKind === "other" ? "other" : "regularFile",
              readable: event.readable !== false,
              ...(event.destination || event.name !== destinationName ? { destination: event.destination ?? event.name } : {}),
              collision: event.collision === true || event.collisionRenamed === true,
              ...(event.cleanupError ? { cleanupError: event.cleanupError } : {}),
            });
            if (response.cleanupError) throw new Error(`Upload preflight cleanup failed: ${response.cleanupError}`);
            settled = true;
            globalThis.clearTimeout(deadline);
            signal?.removeEventListener("abort", abort);
            resolve(response);
          } else if (progress.state === "failed" || progress.state === "cancelled") {
            finishError(new Error(progress.error || (progress.state === "cancelled" ? "Upload preflight cancelled." : "Upload preflight failed.")));
          }
        } catch (error) { finishError(error); }
      };
      if (signal?.aborted) { abort(); return; }
      void invoke<string>("start_terminal_upload_preflight", {
        ...wireScope(scope), sourcePath, destinationName, ...options, onEvent,
      }).then((id) => {
        preflightId = id;
        if (signal?.aborted || settled) cancel();
      }).catch(finishError);
    });
  }

  start(
    scope: TerminalTransferScope,
    sourcePath: string,
    destinationName: string,
    options: UploadOptions,
    onProgress: (progress: TerminalTransferProgress) => void,
  ): Promise<VerifiedTerminalUpload> {
    return new Promise((resolve, reject) => {
      let settled = false;
      let eventSeen = false;
      const onEvent = new Channel<WireUploadEvent>();
      onEvent.onmessage = (event) => {
        if (settled) return;
        eventSeen = true;
        try {
          const progress = mapUploadEvent(event, scope);
          onProgress(progress);
          if (progress.state === "completed") {
            if (!progress.destination || !progress.digest) throw new Error("Upload completed without a verified destination and digest.");
            settled = true;
            resolve({ id: progress.id, destination: progress.destination, digest: progress.digest });
          } else if (progress.state === "failed" || progress.state === "cancelled") {
            settled = true;
            reject(new Error(progress.error || (progress.state === "cancelled" ? "Upload cancelled." : "Upload failed.")));
          }
        } catch (error) {
          settled = true;
          reject(error);
        }
      };
      void invoke<string>("start_terminal_upload", {
        ...wireScope(scope), sourcePath, destinationName, ...options, onEvent,
      }).then((transferId) => {
        if (!settled && !eventSeen) onProgress({
          id: transferId, sourcePath, name: destinationName, state: "queued", completedBytes: "0",
        });
      }).catch((error) => {
        if (settled) return;
        settled = true;
        reject(error instanceof Error ? error : new Error(String(error)));
      });
    });
  }

  async cancel(transferId: string): Promise<TransferCancelDisposition> {
    const response = await invoke<WireCancelDisposition>("cancel_terminal_upload", { transferId });
    if (response.disposition === "cancelRequested") {
      if (response.phase === "queued" || response.phase === "running") return { disposition: response.disposition, phase: response.phase };
      throw new Error(`Invalid cancellation phase for cancelRequested: ${response.phase}`);
    }
    if (response.disposition === "awaitingAuthoritativeOutcome" && response.phase === "verifying") {
      return { disposition: response.disposition, phase: response.phase };
    }
    throw new Error(`Unknown upload cancellation disposition: ${response.disposition}/${response.phase}`);
  }

  async stageClipboardPng(bytes: Uint8Array) {
    const staged = await invoke<{ path: string; sizeBytes: string; name: string }>("stage_clipboard_png", bytes);
    decimalBytes(String(staged.sizeBytes), "staged PNG size");
    return { path: staged.path, sizeBytes: String(staged.sizeBytes), name: validateClipboardDestinationName(staged.name) };
  }
}

function wireScope(scope: TerminalTransferScope) {
  return {
    clientId: scope.clientId,
    profileId: scope.hostProfileId,
    expectedServerIdentity: scope.serverIdentity,
    connectionEpoch: scope.connectionEpoch,
  };
}

function mapUploadEvent(event: WireUploadEvent, scope: TerminalTransferScope): TerminalTransferProgress {
  if (event.serverIdentity !== scope.serverIdentity || event.expectedServerIdentity !== scope.serverIdentity || event.connectionEpoch !== scope.connectionEpoch) {
    throw new Error("Host returned a terminal transfer event for a stale connection scope.");
  }
  const state: TerminalTransferState = transferStateFromWire(event.state);
  if (event.terminal !== isTerminalTransferState(state)) {
    throw new Error("Host returned an inconsistent terminal transfer marker.");
  }
  const completedBytes = String(event.transferredBytes ?? "0");
  decimalBytes(completedBytes, "uploaded byte count");
  if (event.totalBytes !== undefined) decimalBytes(String(event.totalBytes), "upload total");
  const throughput = event.throughputBytesPerSecond === undefined
    ? undefined
    : typeof event.throughputBytesPerSecond === "number"
      ? Number.isFinite(event.throughputBytesPerSecond) && event.throughputBytesPerSecond >= 0
        ? String(Math.floor(event.throughputBytesPerSecond))
        : undefined
      : event.throughputBytesPerSecond;
  if (throughput !== undefined) decimalBytes(throughput, "upload throughput");
  const outcome = transferOutcomeFromWire(event.outcome, state);
  const failureKind = transferFailureKindFromWire(event.failureKind);
  const cleanupStatus = transferCleanupStatusFromWire(event.cleanupStatus);
  validateTransferStateOutcome(state, outcome, failureKind);
  return {
    id: event.transferId,
    sourcePath: event.sourcePath,
    name: event.name,
    state,
    ...(outcome ? { outcome } : {}),
    ...(failureKind ? { failureKind } : {}),
    completedBytes,
    ...(event.totalBytes !== undefined ? { totalBytes: String(event.totalBytes) } : {}),
    ...(throughput !== undefined ? { bytesPerSecond: throughput } : {}),
    ...(event.etaSeconds !== undefined && Number.isFinite(event.etaSeconds) ? { etaSeconds: event.etaSeconds } : {}),
    ...(event.destination ? { destination: event.destination } : {}),
    ...(event.blake3 ? { digest: event.blake3 } : {}),
    ...(event.error ? { error: event.error } : {}),
    ...(event.cleanupError ? { cleanupError: event.cleanupError } : {}),
    ...(cleanupStatus ? { cleanupStatus } : {}),
  };
}

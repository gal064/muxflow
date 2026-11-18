// Sends one file to the host's upload folder (docs/design/mobile-chat-plan.md
// §3 "Attachments"), the way the desktop does
// (crates/desktop-core/src/connection/files/upload_manager.rs):
//
//   PREPARE_TERMINAL_UPLOAD        reserves the name, renaming on a collision
//   WRITE_TERMINAL_UPLOAD_CHUNK    at most 1 MiB each, every one acknowledged
//   COMMIT_TERMINAL_UPLOAD         with the BLAKE3 of the bytes; answers the path
//
// All of it on one bulk connection: the host keeps an upload with the
// connection that prepared it, and removes the partial when that connection
// goes. So a failed or cancelled upload sends CANCEL_TERMINAL_UPLOAD while the
// connection still works, and the connection is closed either way.
//
// The desktop's fourth step is left out. When the COMMIT answer is lost, the
// desktop asks on a fresh connection whether the file was published
// (RECONCILE_TERMINAL_UPLOAD). The phone fails the upload instead and the user
// attaches the file again. A file that was published after all is one nobody
// names, and the host's own retention removes it from the upload folder.

import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import type { Request, Response } from "../../../protocol/gen/envelope_pb";
import { ConnectionClosedError, HostError, RequestTimeoutError } from "../../../protocol/HostConnection";
import {
  cancelTerminalUpload,
  commitTerminalUpload,
  prepareTerminalUpload,
  UPLOAD_CHUNK_BYTES,
  writeTerminalUploadChunk,
} from "../../../protocol/requests";
import { mediaTypeForName } from "./mediaType";

/** The largest image, as on the desktop (`MAX_IMAGE_BYTES` in apps/desktop-gpui/src/chat/attach.rs). */
export const MAX_IMAGE_UPLOAD_BYTES = 25 * 1024 * 1024;
/** The largest file of any kind the phone sends. Its own cap; the host's is far higher. */
export const MAX_UPLOAD_BYTES = 100 * 1024 * 1024;

// A phone link is slow, and one chunk is a megabyte: the connection's 20 s
// default would fail an upload that is only slow. PREPARE also sweeps the
// upload folder's old files before it answers.
export const PREPARE_TIMEOUT_MS = 60_000;
export const CHUNK_TIMEOUT_MS = 120_000;
export const COMMIT_TIMEOUT_MS = 120_000;
/** Short on purpose: closing the connection removes the partial too. */
export const CANCEL_TIMEOUT_MS = 5_000;

/** The bytes to send. `read` answers exactly `length` bytes from `offset`; fewer means the file changed. */
export interface ByteSource {
  name: string;
  size: number;
  read(offset: number, length: number): Promise<Uint8Array>;
}

/** What the upload needs from a bulk connection. `HostConnection` is one. */
export interface UploadLane {
  request(request: Request, options?: { timeoutMs?: number }): Promise<Response>;
  disconnect(): void;
}

export interface UploadFileOptions {
  /** Opens the bulk connection this upload runs on, and closes when it ends. The app passes `openBulkConnection`. */
  openLane: () => Promise<UploadLane>;
  source: ByteSource;
  signal: AbortSignal;
  /** Bytes the host has acknowledged, out of `totalBytes`. */
  onProgress?: (sentBytes: number, totalBytes: number) => void;
  /** Must answer a UUID: the host refuses any other transfer id. */
  newTransferId?: () => string;
}

export interface UploadedFile {
  /** Where the file is on the host; SEND names it by this. */
  hostPath: string;
  /** The name it has there, which differs from the source's when that was taken. */
  name: string;
  sizeBytes: number;
}

/** Why an upload did not finish. `message` is a sentence to show: the host's own words where it gave any. */
export class UploadError extends Error {
  constructor(message: string, readonly aborted = false) {
    super(message);
    this.name = "UploadError";
  }
}

/** Why the phone will not send this file at all, or undefined when it will. */
export function uploadRefusal(name: string, sizeBytes: number): string | undefined {
  if (!Number.isSafeInteger(sizeBytes) || sizeBytes < 0) return "That file could not be read.";
  if (sizeBytes > MAX_UPLOAD_BYTES) return "That file is too large to send from the phone.";
  // The desktop's sentence.
  if (mediaTypeForName(name) !== "" && sizeBytes > MAX_IMAGE_UPLOAD_BYTES) return `${name} is over 25 MB, the most an image can be.`;
  return undefined;
}

/**
 * Uploads `source` and resolves once the host has verified and published it.
 * Rejects with an `UploadError`; `aborted` is set when `signal` ended it.
 */
export async function uploadFile(options: UploadFileOptions): Promise<UploadedFile> {
  const { source, signal } = options;
  const refusal = uploadRefusal(source.name, source.size);
  if (refusal !== undefined) throw new UploadError(refusal);
  if (signal.aborted) throw aborted();

  const total = source.size;
  const transferId = (options.newTransferId ?? newTransferId)();
  const opening = options.openLane();
  let lane: UploadLane | undefined;
  // From the moment PREPARE is on its way the host may hold a partial, even
  // when the answer never arrives.
  let prepared = false;
  try {
    lane = await abortable(opening, signal);
    const open = lane;
    const ask = (request: Request, timeoutMs: number): Promise<Response> => abortable(open.request(request, { timeoutMs }), signal);

    prepared = true;
    const reserved = (await ask(prepareTerminalUpload(transferId, source.name, BigInt(total)), PREPARE_TIMEOUT_MS)).file?.upload;
    if (!reserved) throw new UploadError("The host did not accept the upload.");

    // Hashed as it is read: only the chunk on its way is ever held.
    const hasher = blake3.create();
    let offset = 0;
    while (offset < total) {
      const length = Math.min(UPLOAD_CHUNK_BYTES, total - offset);
      const chunk = await abortable(source.read(offset, length), signal);
      if (chunk.length !== length) throw new UploadError("The file changed while it was being sent.");
      hasher.update(chunk);
      const next = offset + length;
      const accepted = (await ask(writeTerminalUploadChunk(transferId, BigInt(offset), BigInt(total), chunk), CHUNK_TIMEOUT_MS)).file?.transferChunk;
      if (!accepted || accepted.offset !== BigInt(next)) throw new UploadError("The host lost track of the upload.");
      offset = next;
      options.onProgress?.(offset, total);
    }

    const digest = bytesToHex(hasher.digest());
    // An answer lost here leaves the outcome unknown; see the note at the top.
    const published = (await ask(commitTerminalUpload(transferId, BigInt(total), digest), COMMIT_TIMEOUT_MS)).file?.upload;
    // The desktop's rule (`emit_verified_upload`): the host's word that it
    // verified, about the same bytes.
    if (!published?.verified || published.totalBytes !== BigInt(total) || published.blake3 !== digest || published.finalPath === "") {
      throw new UploadError("The host could not verify the upload.");
    }
    return { hostPath: published.finalPath, name: published.destinationName, sizeBytes: total };
  } catch (error) {
    if (lane && prepared && !(error instanceof ConnectionClosedError)) {
      // An upload the host no longer holds answers this without complaint.
      await lane.request(cancelTerminalUpload(transferId), { timeoutMs: CANCEL_TIMEOUT_MS }).catch(() => undefined);
    }
    throw signal.aborted ? aborted() : uploadError(error);
  } finally {
    if (lane) lane.disconnect();
    // Cancelled while the connection was still opening: close it when it arrives.
    else opening.then((late) => late.disconnect(), () => undefined);
  }
}

function aborted(): UploadError {
  return new UploadError("The upload was cancelled.", true);
}

function uploadError(error: unknown): UploadError {
  if (error instanceof UploadError) return error;
  if (error instanceof HostError && error.message !== "") return new UploadError(error.message);
  if (error instanceof RequestTimeoutError) return new UploadError("The host didn't answer in time.");
  if (error instanceof ConnectionClosedError) return new UploadError("The connection to the host was lost.");
  if (error instanceof Error && error.message !== "") return new UploadError(error.message);
  return new UploadError("The upload failed.");
}

/** Settles as `promise` does, or rejects the moment `signal` aborts. The request itself cannot be recalled. */
function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) {
    promise.catch(() => undefined);
    return Promise.reject(aborted());
  }
  return new Promise<T>((resolve, reject) => {
    const onAbort = (): void => reject(aborted());
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener("abort", onAbort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener("abort", onAbort);
        reject(error);
      },
    );
  });
}

/**
 * A UUID, which the host requires of a transfer id. Hermes has no
 * `crypto.randomUUID`; the id only has to be unlike the user's other
 * transfers, so `Math.random` is enough.
 */
export function newTransferId(): string {
  const uuid = globalThis.crypto?.randomUUID?.bind(globalThis.crypto);
  if (uuid) return uuid();
  const hex = (length: number): string => Array.from({ length }, () => Math.floor(Math.random() * 16).toString(16)).join("");
  return `${hex(8)}-${hex(4)}-4${hex(3)}-${(8 + Math.floor(Math.random() * 4)).toString(16)}${hex(3)}-${hex(12)}`;
}

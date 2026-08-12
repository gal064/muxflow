import type { TransferCleanupStatus, TransferFailureKind, TransferOutcome, TransferState } from "../transfers/transferState";

export const LARGE_UPLOAD_BYTES = 500n * 1024n * 1024n;
export const MAX_CLIPBOARD_PNG_BYTES = 25n * 1024n * 1024n;
export const MAX_CLIPBOARD_IMAGE_DIMENSION = 8192;
export const MAX_CLIPBOARD_IMAGE_PIXELS = 16_777_216;
const MAX_IMAGE_HEADER_BYTES = 1024 * 1024;
const SUPPORTED_CLIPBOARD_IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp"] as const;

export type UploadCollisionPolicy = "fail" | "rename" | "overwriteConfirmed";

export interface TerminalTransferConnectionScope {
  clientId: string;
  hostProfileId: string;
  serverIdentity: string;
  connectionEpoch: string;
  mode: "local" | "ssh";
}

export interface TerminalTransferScope extends TerminalTransferConnectionScope {
  paneId: string;
  renderLifetime: string;
}

export interface LocalTerminalPathInspection {
  path: string;
  sizeBytes: string;
  name: string;
}

export interface UploadPreflight {
  sourcePath: string;
  name: string;
  sizeBytes: string;
  sourceKind: "regularFile" | "directory" | "other";
  readable: boolean;
  destination?: string;
  collision: boolean;
  cleanupError?: string;
}

export interface TransferCancelDisposition {
  disposition: "cancelRequested" | "awaitingAuthoritativeOutcome";
  phase: "queued" | "running" | "verifying";
}

export type TerminalTransferState = TransferState;

export interface TerminalTransferProgress {
  id: string;
  sourcePath: string;
  name: string;
  state: TerminalTransferState;
  outcome?: TransferOutcome;
  failureKind?: TransferFailureKind;
  completedBytes: string;
  totalBytes?: string;
  bytesPerSecond?: string;
  etaSeconds?: number;
  destination?: string;
  digest?: string;
  error?: string;
  cleanupError?: string;
  cleanupStatus?: TransferCleanupStatus;
}

export interface VerifiedTerminalUpload {
  id: string;
  destination: string;
  digest: string;
}

export type NativeTerminalClipboard =
  | { kind: "files"; uris: string[] }
  | { kind: "image"; staged: { path: string; sizeBytes: string; name: string } };

export interface TerminalTransferClient {
  readNativeClipboard?(): Promise<NativeTerminalClipboard | undefined>;
  inspectLocalPaths(paths: readonly string[]): Promise<LocalTerminalPathInspection[]>;
  preflight(
    scope: TerminalTransferScope,
    sourcePath: string,
    destinationName: string,
    options: { collision: UploadCollisionPolicy; largeUploadConfirmed: boolean; imagePng: boolean },
    onProgress?: (progress: TerminalTransferProgress) => void,
    signal?: AbortSignal,
  ): Promise<UploadPreflight>;
  start(
    scope: TerminalTransferScope,
    sourcePath: string,
    destinationName: string,
    options: { collision: UploadCollisionPolicy; largeUploadConfirmed: boolean; imagePng: boolean },
    onProgress: (progress: TerminalTransferProgress) => void,
  ): Promise<VerifiedTerminalUpload>;
  cancel(transferId: string): Promise<TransferCancelDisposition>;
  stageClipboardPng(bytes: Uint8Array): Promise<{ path: string; sizeBytes: string; name: string }>;
}

export function validateClipboardDestinationName(name: string): string {
  if (!/^clipboard-[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.png$/u.test(name)) {
    throw new Error("The clipboard staging service did not return a unique agent-compatible PNG destination.");
  }
  return name;
}

export function sameTerminalTransferScope(
  left: TerminalTransferScope | undefined,
  right: TerminalTransferScope | undefined,
): boolean {
  return left === right || Boolean(left && right
    && left.clientId === right.clientId
    && left.hostProfileId === right.hostProfileId
    && left.serverIdentity === right.serverIdentity
    && left.connectionEpoch === right.connectionEpoch
    && left.mode === right.mode
    && left.paneId === right.paneId
    && left.renderLifetime === right.renderLifetime);
}

export function validateLocalTerminalPathInspection(
  item: LocalTerminalPathInspection,
  requestedPath: string,
): LocalTerminalPathInspection {
  decimalBytes(item.sizeBytes, "local file size");
  if (!requestedPath.startsWith("/") || requestedPath.includes("\0")) {
    throw new Error("Local terminal file paths must be absolute and NUL-free.");
  }
  if (item.path !== requestedPath || !item.name || item.name.includes("\0")) {
    throw new Error("The local file inspection response did not match the requested path.");
  }
  return item;
}

export function decimalBytes(value: string, field = "byte count"): bigint {
  if (!/^(0|[1-9]\d*)$/.test(value)) throw new Error(`Invalid ${field} from transfer service.`);
  return BigInt(value);
}

export function requiresLargeUploadConfirmation(items: readonly UploadPreflight[]): boolean {
  return items.some((item) => decimalBytes(item.sizeBytes, "source size") > LARGE_UPLOAD_BYTES);
}

export function validateUploadPreflight(item: UploadPreflight): UploadPreflight {
  decimalBytes(item.sizeBytes, "source size");
  if (!item.sourcePath || item.sourcePath.includes("\0")) throw new Error("The upload source path is invalid.");
  if (item.sourceKind === "directory") throw new Error(`Directories cannot be dropped into terminals: ${item.sourcePath}`);
  if (item.sourceKind !== "regularFile") throw new Error(`Only regular files can be dropped into terminals: ${item.sourcePath}`);
  if (!item.readable) throw new Error(`The source file is not readable: ${item.sourcePath}`);
  return item;
}

/** POSIX single-quote escaping. A leading-dash relative path is made explicitly relative. */
export function shellEscapePath(path: string): string {
  if (!path || path.includes("\0")) throw new Error("Cannot paste an empty or NUL-containing path.");
  const safePath = path.startsWith("-") && !path.includes("/") ? `./${path}` : path;
  return `'${safePath.replaceAll("'", `'"'"'`)}'`;
}

export function joinShellEscapedPaths(paths: readonly string[]): string {
  if (paths.length === 0) throw new Error("No file paths were provided.");
  return paths.map(shellEscapePath).join(" ");
}

/** Image staging paths are intentionally raw for Codex/Claude attachment recognition. */
export function validateAgentImagePath(path: string): string {
  if (!path.startsWith("/") || /[\0-\x20\x7f'"\\]/u.test(path)) {
    throw new Error("The staged image path is not agent-compatible.");
  }
  return path;
}

export function parseFileUriList(value: string): string[] {
  const paths: string[] = [];
  for (const rawLine of value.split(/\r?\n/u)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    let url: URL;
    try { url = new URL(line); } catch { throw new Error("The clipboard file list contains an invalid URI."); }
    if (url.protocol !== "file:" || (url.hostname && url.hostname !== "localhost")) {
      throw new Error("Only local file URIs can be pasted into a terminal.");
    }
    paths.push(decodeURIComponent(url.pathname));
  }
  return paths;
}

export function parseCopiedFileList(value: string): string[] {
  const lines = value.split(/\r?\n/u);
  if (lines[0] === "copy" || lines[0] === "cut") lines.shift();
  return parseFileUriList(lines.join("\n"));
}

export function assertClipboardPngSize(size: number | bigint): void {
  if (BigInt(size) > MAX_CLIPBOARD_PNG_BYTES) {
    throw new Error("The encoded PNG is larger than the 25 MiB clipboard-image limit.");
  }
}

export function supportedClipboardImageType(types: readonly string[]): string | undefined {
  for (const type of types) {
    const normalized = type.toLowerCase();
    if (SUPPORTED_CLIPBOARD_IMAGE_TYPES.some((supported) => supported === normalized)) return normalized;
  }
  return undefined;
}

export async function validateClipboardImageSource(file: Blob): Promise<{ width: number; height: number }> {
  const type = supportedClipboardImageType([file.type]);
  if (!type) throw new Error("Clipboard images must be PNG, JPEG, or WebP.");
  if (file.size === 0) throw new Error("The clipboard image is empty.");
  if (BigInt(file.size) > MAX_CLIPBOARD_PNG_BYTES) {
    throw new Error("The source image is larger than the 25 MiB clipboard-image limit.");
  }
  const header = new Uint8Array(await file.slice(0, Math.min(file.size, MAX_IMAGE_HEADER_BYTES)).arrayBuffer());
  const dimensions = type === "image/png" ? pngDimensions(header)
    : type === "image/jpeg" ? jpegDimensions(header)
      : webpDimensions(header);
  if (!dimensions) throw new Error(`The ${type.slice("image/".length).toUpperCase()} clipboard image header is invalid or its dimensions cannot be safely determined.`);
  assertSafeImageDimensions(dimensions.width, dimensions.height);
  return dimensions;
}

function assertSafeImageDimensions(width: number, height: number): void {
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height) || width <= 0 || height <= 0
    || width > MAX_CLIPBOARD_IMAGE_DIMENSION || height > MAX_CLIPBOARD_IMAGE_DIMENSION
    || width * height > MAX_CLIPBOARD_IMAGE_PIXELS) {
    throw new Error(`Clipboard image dimensions exceed the ${MAX_CLIPBOARD_IMAGE_DIMENSION}px / ${MAX_CLIPBOARD_IMAGE_PIXELS}-pixel safety limit.`);
  }
}

function pngDimensions(bytes: Uint8Array): { width: number; height: number } | undefined {
  if (bytes.length < 24 || ![137, 80, 78, 71, 13, 10, 26, 10].every((value, index) => bytes[index] === value)
    || String.fromCharCode(...bytes.subarray(12, 16)) !== "IHDR") return undefined;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return { width: view.getUint32(16, false), height: view.getUint32(20, false) };
}

function jpegDimensions(bytes: Uint8Array): { width: number; height: number } | undefined {
  if (bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) return undefined;
  const sof = new Set([0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf]);
  let offset = 2;
  while (offset + 3 < bytes.length) {
    while (bytes[offset] === 0xff) offset += 1;
    const marker = bytes[offset++];
    if (marker === undefined || marker === 0xd9 || marker === 0xda) return undefined;
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
    if (offset + 1 >= bytes.length) return undefined;
    const length = bytes[offset] * 256 + bytes[offset + 1];
    if (length < 2 || offset + length > bytes.length) return undefined;
    if (sof.has(marker)) {
      if (length < 7) return undefined;
      return {
        height: bytes[offset + 3] * 256 + bytes[offset + 4],
        width: bytes[offset + 5] * 256 + bytes[offset + 6],
      };
    }
    offset += length;
  }
  return undefined;
}

function webpDimensions(bytes: Uint8Array): { width: number; height: number } | undefined {
  if (bytes.length < 16 || ascii(bytes, 0, 4) !== "RIFF" || ascii(bytes, 8, 12) !== "WEBP") return undefined;
  const chunk = ascii(bytes, 12, 16);
  if (chunk === "VP8X" && bytes.length >= 30) return { width: uint24le(bytes, 24) + 1, height: uint24le(bytes, 27) + 1 };
  if (chunk === "VP8L" && bytes.length >= 25 && bytes[20] === 0x2f) return {
    width: 1 + bytes[21] + ((bytes[22] & 0x3f) << 8),
    height: 1 + ((bytes[22] & 0xc0) >> 6) + (bytes[23] << 2) + ((bytes[24] & 0x0f) << 10),
  };
  if (chunk === "VP8 " && bytes.length >= 30 && bytes[23] === 0x9d && bytes[24] === 0x01 && bytes[25] === 0x2a) return {
    width: (bytes[26] + (bytes[27] << 8)) & 0x3fff,
    height: (bytes[28] + (bytes[29] << 8)) & 0x3fff,
  };
  return undefined;
}

function ascii(bytes: Uint8Array, start: number, end: number): string {
  return String.fromCharCode(...bytes.subarray(start, end));
}

function uint24le(bytes: Uint8Array, offset: number): number {
  return bytes[offset] + (bytes[offset + 1] << 8) + (bytes[offset + 2] << 16);
}

export async function encodeImageAsPng(file: Blob): Promise<Uint8Array> {
  await validateClipboardImageSource(file);
  const bitmap = await createImageBitmap(file);
  try {
    assertSafeImageDimensions(bitmap.width, bitmap.height);
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("PNG encoding is unavailable in this WebView.");
    context.drawImage(bitmap, 0, 0);
    const png = await new Promise<Blob>((resolve, reject) => canvas.toBlob(
      (encoded) => encoded ? resolve(encoded) : reject(new Error("PNG encoding failed.")),
      "image/png",
    ));
    assertClipboardPngSize(png.size);
    return new Uint8Array(await png.arrayBuffer());
  } finally {
    bitmap.close();
  }
}

export async function uploadInOriginalOrder(
  items: readonly UploadPreflight[],
  start: (
    item: UploadPreflight,
    index: number,
    onProgress: (progress: TerminalTransferProgress) => void,
  ) => Promise<VerifiedTerminalUpload>,
  cancel: (transferId: string) => Promise<unknown>,
  onProgress: (progress: TerminalTransferProgress) => void,
  signal?: AbortSignal,
): Promise<VerifiedTerminalUpload[]> {
  const latest = new Map<number, TerminalTransferProgress>();
  const cancelRequested = new Set<string>();
  let aborting = false;
  let cancellation = Promise.resolve();
  const terminal = (state: TerminalTransferState) => state === "completed" || state === "cancelled" || state === "failed";
  const cancelNonterminal = () => {
    cancellation = cancellation.then(async () => {
      const ids = [...latest.values()]
        .filter((progress) => !terminal(progress.state) && !cancelRequested.has(progress.id))
        .map((progress) => progress.id);
      for (const id of ids) cancelRequested.add(id);
      await Promise.allSettled(ids.map((id) => cancel(id)));
    });
    return cancellation;
  };
  const abort = () => {
    aborting = true;
    void cancelNonterminal();
  };
  if (signal?.aborted) abort();
  signal?.addEventListener("abort", abort, { once: true });
  const promises = items.map((item, index) => start(item, index, (progress) => {
    latest.set(index, progress);
    onProgress(progress);
    if (aborting && !terminal(progress.state)) void cancelNonterminal();
  }).catch(async (error) => {
    if (!aborting) aborting = true;
    await cancelNonterminal();
    throw error;
  }));
  const settled = await Promise.allSettled(promises);
  signal?.removeEventListener("abort", abort);
  if (signal?.aborted) {
    aborting = true;
    await cancelNonterminal();
    throw new DOMException("Terminal transfer scope changed.", "AbortError");
  }
  const failed = settled.find((result): result is PromiseRejectedResult => result.status === "rejected");
  if (failed) {
    aborting = true;
    await cancelNonterminal();
    // Every sibling has now reached its terminal event (and therefore carries
    // the backend's cleanup result) before the UI unlocks the pane.
    throw failed.reason;
  }
  // allSettled retains input order even though the backend's two active slots
  // are free to complete in any order.
  return settled.map((result) => (result as PromiseFulfilledResult<VerifiedTerminalUpload>).value);
}

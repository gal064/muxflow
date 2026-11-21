// What the reply field's chips and its Send button say about the files
// waiting on it (docs/design/mobile-chat-mock.html, S7). The rules and the
// words are the desktop's (apps/desktop-gpui/src/chat/attach.rs). Pure.

import type { AttachmentEntry } from "./attachmentQueue";
import { mediaTypeForName } from "./mediaType";

/**
 * Whether the files on the reply field can go: there are `none`, some are
 * still `uploading`, one `failed`, or every one is on the host (`ready`).
 */
export type FilesState = "none" | "uploading" | "failed" | "ready";

export function filesState(entries: readonly AttachmentEntry[]): FilesState {
  if (entries.length === 0) return "none";
  if (entries.some((entry) => entry.status === "failed")) return "failed";
  return entries.every((entry) => entry.status === "ready") ? "ready" : "uploading";
}

/** Why Send is off while a file uploads: the desktop's sentence. */
export const FILES_UPLOADING = "The attachments are still uploading.";
/** Why Send is off after a file failed. The desktop also offers Retry; the phone's chip is removed and attached again. */
export const FILES_FAILED = "An attachment failed to upload. Remove it to send.";

/** Why Send is off because of the files, or undefined when they do not hold it. */
export function filesRefusal(state: FilesState): string | undefined {
  return state === "uploading" ? FILES_UPLOADING : state === "failed" ? FILES_FAILED : undefined;
}

/** `1.4 MB`, `860 KB`: a size as a chip says it, in binary units (the desktop's `size_label`). */
export function sizeLabel(bytes: number | bigint): string {
  const size = Number(bytes);
  const KB = 1024;
  const MB = 1024 * 1024;
  if (size < KB) return `${size} B`;
  if (size < MB) return `${Math.floor((size + KB / 2) / KB)} KB`;
  const tenths = Math.floor((size * 10 + MB / 2) / MB);
  return `${Math.floor(tenths / 10)}.${tenths % 10} MB`;
}

/** How much of an entry the host has, 0 to 1. */
export function uploadShare(entry: Pick<AttachmentEntry, "status" | "sentBytes" | "sizeBytes">): number {
  if (entry.status === "ready") return 1;
  if (entry.sizeBytes <= 0) return 0;
  return Math.min(1, Math.max(0, entry.sentBytes / entry.sizeBytes));
}

/** The line under a chip's name: its size, how far the upload is, or why it failed. */
export function chipLine(entry: AttachmentEntry): string {
  switch (entry.status) {
    case "queued":
      return "Waiting to upload";
    case "uploading":
      return `Uploading ${Math.round(uploadShare(entry) * 100)}%`;
    case "failed":
      return entry.error || "The upload failed.";
    default:
      return sizeLabel(entry.sizeBytes);
  }
}

/** Whether an attachment is drawn as a picture: by the type it was given, else by its name. */
export function isImage(attachment: { mediaType: string; name: string }): boolean {
  return (attachment.mediaType || mediaTypeForName(attachment.name)).startsWith("image/");
}

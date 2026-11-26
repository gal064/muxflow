// What the file viewer shows for one streamed file (design doc §9.7).
//
// Mode selection lives here, away from the screen, because it is the one place
// the document's copy is spelled and the one rule that has to agree with the
// classification the host sent.

import { formatSize, isMarkdownName } from "./entries";
import { MAX_IMAGE_BYTES, type FileBody } from "./fileStream";

export type ViewerMode = "rendered" | "source";

export type FilePresentation =
  /** A Markdown file: §9.7 offers the `Rendered | Source` toggle, Rendered first. */
  | { kind: "markdown"; text: string }
  /** Any other text file: monospace, horizontally scrollable, no wrapping. */
  | { kind: "plain"; text: string }
  /** An image the host streamed, drawn by the phone. */
  | { kind: "image"; bytes: Uint8Array }
  /** A PDF the host streamed, drawn by the phone. */
  | { kind: "pdf"; bytes: Uint8Array }
  /** One of the placeholders §9.7 spells. */
  | { kind: "placeholder"; message: string };

/** §9.7's exact strings. */
export const FILE_VIEWER_COPY = {
  binary: "This is a binary file.",
  unavailable: "Couldn't open this file.",
  tooLarge: (size: string) => `This file is too large to show here (${size}).`,
  /** An image or a PDF over the host's preview limit, which streams no bytes. */
  tooLargeToPreview: (limit: string) => `This file is too large to show here (over ${limit}).`,
  /** The phone has the bytes and cannot draw them (an SVG, a damaged or encrypted PDF). */
  imageNotShown: "This image can't be shown on the phone.",
  pdfNotShown: "This PDF can't be shown on the phone.",
  error: (displayMessage: string) => `Couldn't open this file: ${displayMessage}`,
} as const;

/** §9.6's exact strings. */
export const FILES_COPY = {
  empty: "Empty folder",
  error: (displayMessage: string) => `Couldn't read this folder: ${displayMessage}`,
} as const;

/** Decodes UTF-8 with replacement characters, as §9.7 step 1 asks. */
export function decodeText(bytes: Uint8Array): string {
  return new TextDecoder("utf-8").decode(bytes);
}

/**
 * Mode selection: the host's classification decides whether there is text at
 * all, and the file name decides whether that text is Markdown.
 */
export function filePresentation(body: FileBody, name: string): FilePresentation {
  switch (body.kind) {
    case "text":
      return { kind: isMarkdownName(name) ? "markdown" : "plain", text: decodeText(body.bytes) };
    case "tooLarge":
      return { kind: "placeholder", message: FILE_VIEWER_COPY.tooLarge(formatSize(body.size)) };
    case "binary":
      return { kind: "placeholder", message: FILE_VIEWER_COPY.binary };
    case "image":
    case "pdf":
      return body.bytes
        ? { kind: body.kind, bytes: body.bytes }
        : { kind: "placeholder", message: FILE_VIEWER_COPY.tooLargeToPreview(formatSize(BigInt(MAX_IMAGE_BYTES))) };
    case "unavailable":
      return { kind: "placeholder", message: FILE_VIEWER_COPY.unavailable };
  }
}

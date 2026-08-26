// What the file viewer shows for one streamed file (design doc §9.7).
//
// Mode selection lives here, away from the screen, because it is the one place
// the document's copy is spelled and the one rule that has to agree with the
// classification the host sent.

import { formatSize, isMarkdownName } from "./entries";
import type { FileBody } from "./fileStream";

export type ViewerMode = "rendered" | "source";

export type FilePresentation =
  /** A Markdown file: §9.7 offers the `Rendered | Source` toggle, Rendered first. */
  | { kind: "markdown"; text: string }
  /** Any other text file: monospace, horizontally scrollable, no wrapping. */
  | { kind: "plain"; text: string }
  /** One of the four placeholders §9.7 spells. */
  | { kind: "placeholder"; message: string };

/** §9.7's exact strings. */
export const FILE_VIEWER_COPY = {
  binary: "This is a binary file.",
  image: "Images aren't shown in this version.",
  unavailable: "Couldn't open this file.",
  tooLarge: (size: string) => `This file is too large to show here (${size}).`,
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
      return { kind: "placeholder", message: FILE_VIEWER_COPY.image };
    case "unavailable":
      return { kind: "placeholder", message: FILE_VIEWER_COPY.unavailable };
  }
}

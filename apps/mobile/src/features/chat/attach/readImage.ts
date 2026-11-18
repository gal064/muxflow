// Reads an attached image back from the host, for showing it full screen
// (docs/design/mobile-chat-plan.md §3 "Attachments"). No host change: the
// path is resolved the way a path tapped in a terminal is
// (RESOLVE_TERMINAL_FILE answers a capability for that one file), and the
// bytes come as the file viewer's do (OPEN_FILE_STREAM on a bulk connection).
// Only the whole file can be read, so nothing calls this unasked.
//
// The host calls a file an image by its extension: png, jpg, jpeg, gif, webp,
// bmp and svg (`image_mime` in apps/host/src/service/filesystem.rs). A HEIC
// is not one of them and is refused here as not an image.

import type { Request, Response } from "../../../protocol/gen/envelope_pb";
import { rooted } from "../../files/activeRoot";
import { readFile, type OpenFileStreamFn } from "../../files/fileStream";
import { resolveTerminalFile, type TerminalFileCapture } from "../../files/terminalFile";
import { mediaTypeForName } from "./mediaType";

/** The chat's pane as the phone sees it now: `{ pane, serverIdentity, topologyGeneration }` from the session store. */
export type AttachmentPane = TerminalFileCapture;

export interface ImageReadLanes {
  /** The control connection: it answers RESOLVE_TERMINAL_FILE. */
  control: { request(request: Request): Promise<Response> };
  /** A bulk connection, the only kind that serves a file's bytes. The app passes `openBulkConnection`; it is left open. */
  openBulk: () => Promise<{ request: OpenFileStreamFn }>;
}

export interface AttachedImage {
  bytes: Uint8Array;
  /** Such as "image/png". */
  mediaType: string;
  name: string;
}

/** Why an attachment cannot be shown, as a sentence to show. A refusal by the host arrives as its own `HostError`. */
export class AttachedImageError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AttachedImageError";
  }
}

/**
 * The bytes of the image at `hostPath`, an absolute path on the host such as
 * `ChatAttachment.path`. Refuses a file that is not an image, and an image
 * over 25 MiB, which the host does not stream.
 */
export async function readAttachedImage(lanes: ImageReadLanes, pane: AttachmentPane, hostPath: string): Promise<AttachedImage> {
  // An image the agent's record holds inline has no path.
  if (!hostPath.startsWith("/")) throw new AttachedImageError("That image is not a file on the host.");
  const resolved = await resolveTerminalFile((request) => lanes.control.request(request), hostPath, pane);
  const bulk = await lanes.openBulk();
  const body = await readFile(
    (request, options) => bulk.request(request, options),
    rooted(resolved.root, resolved.path),
    pane.serverIdentity,
    { keepImages: true },
  );
  if (body.kind !== "image") throw new AttachedImageError("That attachment is not an image.");
  if (!body.bytes) throw new AttachedImageError("That image is too large to show.");
  return {
    bytes: body.bytes,
    mediaType: body.metadata?.mime || mediaTypeForName(resolved.name),
    name: resolved.name,
  };
}

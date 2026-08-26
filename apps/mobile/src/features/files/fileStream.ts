// Reading one file (design doc §9.7 step 1, §11.1).
//
// `OPEN_FILE_STREAM` answers on its own request id with exactly one header
// frame, then zero or more body frames, then the request's Response. The
// assembler below owns that shape; `HostConnection` only routes the frames.
//
// File bodies are served **only on a bulk connection**: the control lane
// answers `bulk_connection_required` (`Lane::Bulk` in
// apps/host/src/service/requests/operation_policy.rs), so the caller passes the
// bulk lane from the connection manager.

import {
  FileContentKind,
  type FileMetadata,
  type FileStreamFrame,
  type FileStreamHeader,
  type Response,
} from "../../protocol/gen/envelope_pb";
import { newOperationId, openFileStream, type RootedPath } from "../../protocol/requests";

/**
 * §11.1: the phone will not read a body larger than this. The host's own text
 * limit is 10 MiB (`MAX_TEXT_BYTES`), which is more than a phone should hold in
 * a JS string, so anything above the cap is presented as too large locally.
 */
export const MAX_CLIENT_FILE_BYTES = 2 * 1024 * 1024;

export type FileBody =
  | { kind: "text"; bytes: Uint8Array; metadata: FileMetadata | undefined }
  /** Too large for the host (`FILE_CONTENT_KIND_TOO_LARGE`) or past the phone's own cap. */
  | { kind: "tooLarge"; size: bigint }
  | { kind: "binary" }
  | { kind: "image" }
  /** `UNSPECIFIED`, a missing header, or text the host declined to stream. */
  | { kind: "unavailable" };

export class FileStreamError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "FileStreamError";
  }
}

/**
 * Assembles one `OPEN_FILE_STREAM` answer.
 *
 * Body frames carry their own `offset`, so they are written into a buffer sized
 * from the header rather than concatenated in arrival order: §11.1 says the
 * frames may interleave with unrelated traffic and says nothing about their
 * order relative to each other.
 */
export class FileStreamAssembler {
  private header: FileStreamHeader | undefined;
  private buffer: Uint8Array | undefined;
  private received = 0;
  private draining = false;
  private sawEof = false;

  constructor(private readonly operationId: string) {}

  push(frame: FileStreamFrame): void {
    // §9.7 step 2: frames for a different operation are not this file's.
    if (frame.operationId !== this.operationId) return;
    if (frame.header) {
      if (this.header) throw new FileStreamError("the host sent a second stream header");
      this.header = frame.header;
      const total = frame.header.totalBytes;
      // `content_streaming` — not the classification — decides whether body
      // frames follow (`open_stream.rs`). An image under the host's 25 MiB
      // preview limit streams its bytes even though §9.7 only ever shows a
      // placeholder for it, so those frames are drained rather than kept.
      if (frame.header.contentStreaming) {
        const readable = frame.header.contentKind === FileContentKind.TEXT && total <= BigInt(MAX_CLIENT_FILE_BYTES);
        // §11.1: past the cap, do not read the body. Frames still arrive and
        // are dropped so the request id settles on the response.
        if (readable) this.buffer = new Uint8Array(Number(total));
        else this.draining = true;
      }
      return;
    }
    if (!this.header) throw new FileStreamError("a body frame arrived before the stream header");
    if (frame.eof) this.sawEof = true;
    if (this.draining || frame.data.length === 0) return;
    const buffer = this.buffer;
    if (!buffer) {
      // A header that declared no body must not carry one.
      throw new FileStreamError("the host streamed a body for a file it declared unreadable");
    }
    // The host cuts the frames from one buffer in order (`FileStreamBody::chunks`)
    // and writes them down one connection, so the offsets are contiguous.
    // Insisting on that is what makes the length check below a coverage check:
    // a byte count alone would accept two overlapping frames and a zero-filled
    // hole between them.
    const offset = Number(frame.offset);
    if (offset !== this.received || offset + frame.data.length > buffer.length) {
      throw new FileStreamError(`a body frame arrived at ${frame.offset}, expected ${this.received}`);
    }
    buffer.set(frame.data, offset);
    this.received += frame.data.length;
  }

  /** Called once the request's Response has settled. */
  finish(): FileBody {
    const header = this.header;
    if (!header) return { kind: "unavailable" };
    const size = header.metadata?.size ?? header.totalBytes;
    switch (header.contentKind) {
      case FileContentKind.TOO_LARGE:
        return { kind: "tooLarge", size };
      case FileContentKind.BINARY:
        return { kind: "binary" };
      case FileContentKind.IMAGE:
        return { kind: "image" };
      case FileContentKind.TEXT: {
        if (this.draining) return { kind: "tooLarge", size: header.totalBytes };
        const buffer = this.buffer;
        if (!buffer) return { kind: "unavailable" };
        // §9.7 step 1: the assembled length must be the declared length.
        if (this.received !== buffer.length) {
          throw new FileStreamError(`the file ended early: ${this.received} of ${buffer.length} bytes`);
        }
        if (buffer.length > 0 && !this.sawEof) throw new FileStreamError("the file stream ended without an eof frame");
        return { kind: "text", bytes: buffer, metadata: header.metadata };
      }
      default:
        return { kind: "unavailable" };
    }
  }
}

/** The one call this module needs from the bulk `HostConnection`. */
export type OpenFileStreamFn = (
  request: ReturnType<typeof openFileStream>,
  options: { onFileStream: (frame: FileStreamFrame) => void },
) => Promise<Response>;

/** Opens, streams and classifies one file on the bulk lane. */
export async function readFile(
  request: OpenFileStreamFn,
  target: RootedPath,
  expectedServerIdentity: string,
): Promise<FileBody> {
  const operationId = newOperationId();
  const assembler = new FileStreamAssembler(operationId);
  let pushError: unknown;
  await request(openFileStream(operationId, target, expectedServerIdentity), {
    onFileStream: (frame) => {
      // A throw here would escape into the connection's frame loop and be
      // diagnosed as a protocol error, dropping the whole connection. Hold it
      // and re-throw once the response has settled the request id.
      if (pushError !== undefined) return;
      try {
        assembler.push(frame);
      } catch (error) {
        pushError = error;
      }
    },
  });
  if (pushError !== undefined) throw pushError;
  return assembler.finish();
}

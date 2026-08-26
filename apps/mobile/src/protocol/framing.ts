// 4-byte big-endian length prefix + protobuf Envelope body (design doc §7.1).
// One-to-one port of `encode_frame` and `FrameAccumulator` in
// crates/protocol/src/lib.rs.

import { fromBinary, toBinary } from "@bufbuild/protobuf";
import { EnvelopeSchema, type Envelope } from "./gen/envelope_pb";

export const MAX_FRAME_BYTES = 16 * 1024 * 1024;

export class FrameTooLargeError extends Error {
  constructor(readonly length: number) {
    super(`frame length ${length} exceeds the ${MAX_FRAME_BYTES}-byte limit`);
    this.name = "FrameTooLargeError";
  }
}

export function encodeFrame(envelope: Envelope): Uint8Array {
  const body = toBinary(EnvelopeSchema, envelope);
  if (body.byteLength > MAX_FRAME_BYTES) throw new FrameTooLargeError(body.byteLength);
  const frame = new Uint8Array(4 + body.byteLength);
  new DataView(frame.buffer).setUint32(0, body.byteLength, false);
  frame.set(body, 4);
  return frame;
}

/**
 * Incremental decoder. The bound applies to each advertised frame body, never
 * to the aggregate buffered byte count: a read may contain a complete
 * maximum-sized frame followed by part or all of later frames.
 */
export class FrameAccumulator {
  private bytes = new Uint8Array(0);
  private head = 0;

  push(chunk: Uint8Array): void {
    if (chunk.byteLength === 0) return;
    const pending = this.bytes.byteLength - this.head;
    if (pending === 0) {
      this.bytes = chunk.slice();
      this.head = 0;
      return;
    }
    const joined = new Uint8Array(pending + chunk.byteLength);
    joined.set(this.bytes.subarray(this.head), 0);
    joined.set(chunk, pending);
    this.bytes = joined;
    this.head = 0;
  }

  /** Bytes buffered and not yet decoded. */
  get buffered(): number {
    return this.bytes.byteLength - this.head;
  }

  nextFrame(): Envelope | undefined {
    const pending = this.bytes.byteLength - this.head;
    if (pending < 4) return undefined;
    const length = new DataView(this.bytes.buffer, this.bytes.byteOffset + this.head, 4).getUint32(0, false);
    if (length > MAX_FRAME_BYTES) throw new FrameTooLargeError(length);
    if (pending < length + 4) return undefined;
    const start = this.head + 4;
    const envelope = fromBinary(EnvelopeSchema, this.bytes.subarray(start, start + length));
    this.head = start + length;
    if (this.head === this.bytes.byteLength) {
      this.bytes = new Uint8Array(0);
      this.head = 0;
    }
    return envelope;
  }
}

import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { FrameAccumulator, FrameTooLargeError, MAX_FRAME_BYTES, encodeFrame } from "./framing";
import { CancelSchema, RequestSchema, Operation } from "./gen/envelope_pb";
import { hostEnvelope } from "./testing/fakeTransport";

const cancel = (target: bigint, requestId: bigint) =>
  hostEnvelope({ case: "cancel", value: create(CancelSchema, { targetRequestId: target }) }, { requestId });

describe("framing", () => {
  it("round-trips an envelope through a 4-byte big-endian length prefix", () => {
    const frame = encodeFrame(cancel(42n, 77n));
    const length = new DataView(frame.buffer).getUint32(0, false);
    expect(length).toBe(frame.byteLength - 4);
    const decoder = new FrameAccumulator();
    decoder.push(frame);
    const decoded = decoder.nextFrame();
    expect(decoded?.requestId).toBe(77n);
    expect(decoded?.payload.case).toBe("cancel");
    expect(decoder.nextFrame()).toBeUndefined();
    expect(decoder.buffered).toBe(0);
  });

  it("decodes a frame delivered one byte at a time, with empty chunks", () => {
    const encoded = encodeFrame(cancel(42n, 77n));
    const decoder = new FrameAccumulator();
    decoder.push(new Uint8Array(0));
    for (let i = 0; i < encoded.byteLength - 1; i += 1) {
      decoder.push(encoded.subarray(i, i + 1));
      expect(decoder.nextFrame()).toBeUndefined();
    }
    decoder.push(new Uint8Array(0));
    decoder.push(encoded.subarray(encoded.byteLength - 1));
    expect(decoder.nextFrame()?.requestId).toBe(77n);
  });

  it("decodes two frames from one chunk and a frame split across chunks", () => {
    const first = encodeFrame(cancel(1n, 1n));
    const second = encodeFrame(cancel(2n, 2n));
    const third = encodeFrame(cancel(3n, 3n));
    const joined = new Uint8Array(first.byteLength + 1);
    joined.set(first, 0);
    joined.set(second.subarray(0, 1), first.byteLength);
    const decoder = new FrameAccumulator();
    decoder.push(joined);
    expect(decoder.nextFrame()?.requestId).toBe(1n);
    expect(decoder.nextFrame()).toBeUndefined();
    const rest = new Uint8Array(second.byteLength - 1 + third.byteLength);
    rest.set(second.subarray(1), 0);
    rest.set(third, second.byteLength - 1);
    decoder.push(rest);
    expect(decoder.nextFrame()?.requestId).toBe(2n);
    expect(decoder.nextFrame()?.requestId).toBe(3n);
    expect(decoder.nextFrame()).toBeUndefined();
  });

  it("rejects an advertised body longer than 16 MiB before buffering it", () => {
    const decoder = new FrameAccumulator();
    const prefix = new Uint8Array(4);
    new DataView(prefix.buffer).setUint32(0, MAX_FRAME_BYTES + 1, false);
    decoder.push(prefix);
    expect(() => decoder.nextFrame()).toThrow(FrameTooLargeError);
  });

  it("refuses to encode a body over the limit", () => {
    const oversized = hostEnvelope({
      case: "request",
      value: create(RequestSchema, { operation: Operation.TERMINAL_INPUT, data: new Uint8Array(MAX_FRAME_BYTES + 1) }),
    });
    expect(() => encodeFrame(oversized)).toThrow(FrameTooLargeError);
  });

  it("accepts a maximum-sized frame followed by more frames", () => {
    const body = new Uint8Array(MAX_FRAME_BYTES - 64);
    const big = hostEnvelope({ case: "request", value: create(RequestSchema, { operation: Operation.TERMINAL_INPUT, data: body }) }, { requestId: 1n });
    const decoder = new FrameAccumulator();
    const encoded = encodeFrame(big);
    expect(encoded.byteLength - 4).toBeLessThanOrEqual(MAX_FRAME_BYTES);
    decoder.push(encoded);
    decoder.push(encodeFrame(cancel(1n, 2n)));
    expect(decoder.nextFrame()?.requestId).toBe(1n);
    expect(decoder.nextFrame()?.requestId).toBe(2n);
  });
});

import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { capabilityNames, validateHostContract } from "./contract";
import { FrameAccumulator, encodeFrame } from "./framing";
import { EventKind, Operation, ServerHelloSchema, VoiceProvider } from "./gen/envelope_pb";
import vectors from "./testing/rust_vectors.json";

describe("Rust-produced admission contract", () => {
  it.each(vectors.admissions)("major=$major capabilities=$capabilities readOnly=$readOnly", (vector) => {
    const refusal = validateHostContract(vector.major, create(ServerHelloSchema, {
      capabilities: BigInt(vector.capabilities), readOnly: vector.readOnly, incompatibility: "test refusal",
    }));
    expect(refusal?.kind ?? null).toBe(vector.refusal?.kind ?? null);
    if (refusal?.kind === "missingCapabilities") {
      expect(refusal.missing.toString()).toBe(vector.refusal?.missing);
      expect(capabilityNames(refusal.missing)).toEqual(vector.refusal?.names);
    }
  });
});

describe("Rust-produced framed protobuf", () => {
  function decode(name: keyof typeof vectors.frames) {
    const frame = Uint8Array.from(Buffer.from(vectors.frames[name], "hex"));
    const accumulator = new FrameAccumulator();
    // Fragment the Rust prefix and body through the production TS reader.
    for (const byte of frame.subarray(0, -1)) {
      accumulator.push(Uint8Array.of(byte));
      expect(accumulator.nextFrame()).toBeUndefined();
    }
    accumulator.push(frame.subarray(-1));
    const message = accumulator.nextFrame()!;
    expect(message.requestId).toBe(0xffff_ffff_ffff_ffffn);
    expect(message.sequence).toBe((1n << 53n) + 9n);
    expect(encodeFrame(message)).toEqual(frame);
    expect(accumulator.nextFrame()).toBeUndefined();
    return message.payload;
  }

  it("preserves non-UTF8 terminal bytes and a known empty history", () => {
    const payload = decode("terminal");
    expect(payload.case).toBe("terminalBytes");
    if (payload.case !== "terminalBytes") throw new Error("wrong terminal payload");
    expect([...payload.value.data]).toEqual([0, 255, 27, 91, 72]);
    expect(payload.value.generation).toBe((1n << 53n) + 7n);
    expect(payload.value.historySize).toBe(0);
    expect(payload.value.historySizeKnown).toBe(true);
  });

  it("preserves u64 file offsets and opaque file bodies", () => {
    const payload = decode("file");
    expect(payload.case).toBe("fileStream");
    if (payload.case !== "fileStream") throw new Error("wrong file payload");
    expect(payload.value.offset).toBe(0xffff_ffff_ffff_ffffn);
    expect([...payload.value.data]).toEqual([0, 159, 146, 150]);
    expect(payload.value.eof).toBe(true);
    expect(payload.value.blake3).toBe("digest");
  });

  it("preserves raw Git paths and connection identity", () => {
    const payload = decode("git");
    expect(payload.case).toBe("request");
    if (payload.case !== "request") throw new Error("wrong request payload");
    expect(payload.value.operation).toBe(Operation.GIT_MUTATION);
    expect([...payload.value.git!.path]).toEqual([97, 10, 255]);
    expect([...payload.value.git!.originalPath]).toEqual([98, 9, 254]);
    expect(payload.value.git!.connectionEpoch).toBe((1n << 53n) + 9n);
  });

  it("preserves voice audio, provider and reply generation", () => {
    const payload = decode("voice");
    expect(payload.case).toBe("event");
    if (payload.case !== "event") throw new Error("wrong event payload");
    expect(payload.value.kind).toBe(EventKind.VOICE_REPLY);
    const reply = payload.value.voice!.reply!;
    expect([...reply.audio]).toEqual([255, 251, 144]);
    expect(reply.provider).toBe(VoiceProvider.EDGE_TTS);
    expect(reply.stateGeneration).toBe((1n << 53n) + 5n);
  });
});

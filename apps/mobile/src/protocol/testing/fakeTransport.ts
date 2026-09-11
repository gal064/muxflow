// An in-memory Transport that records what the client sent and replays
// scripted host frames. Test-only.

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { FrameAccumulator, encodeFrame } from "../framing";
import {
  EnvelopeSchema,
  Priority,
  ResponseSchema,
  ServerHelloSchema,
  SnapshotSchema,
  type Envelope,
  type Response,
  type ServerHello,
  type Snapshot,
} from "../gen/envelope_pb";
import { HOST_CAPABILITIES, PROTOCOL_MAJOR, PROTOCOL_MINOR } from "../contract";
import type { Transport, TransportClose } from "../Transport";

export class FakeTransport implements Transport {
  readonly sent: Envelope[] = [];
  closed = false;
  private readonly decoder = new FrameAccumulator();
  private dataListener: ((chunk: Uint8Array) => void) | undefined;
  private closedListener: ((close: TransportClose) => void) | undefined;

  write(bytes: Uint8Array): void {
    if (this.closed) throw new Error("write after close");
    this.decoder.push(bytes);
    for (;;) {
      const frame = this.decoder.nextFrame();
      if (!frame) break;
      this.sent.push(frame);
    }
  }

  onData(listener: (chunk: Uint8Array) => void): void {
    this.dataListener = listener;
  }

  onClosed(listener: (close: TransportClose) => void): void {
    this.closedListener = listener;
  }

  close(): void {
    this.closed = true;
  }

  /** Delivers one host frame, optionally split into byte-sized chunks. */
  feed(frame: Envelope, chunkSize?: number): void {
    const bytes = encodeFrame(frame);
    if (chunkSize === undefined) {
      this.dataListener?.(bytes);
      return;
    }
    for (let offset = 0; offset < bytes.byteLength; offset += chunkSize) {
      this.dataListener?.(bytes.subarray(offset, offset + chunkSize));
    }
  }

  feedRaw(bytes: Uint8Array): void {
    this.dataListener?.(bytes);
  }

  /** The remote side going away. */
  closeFromRemote(close: TransportClose): void {
    this.closed = true;
    this.closedListener?.(close);
  }

  /** Frames sent since the last call. */
  drain(): Envelope[] {
    return this.sent.splice(0, this.sent.length);
  }
}

export function hostEnvelope(
  payload: Envelope["payload"],
  options: { requestId?: bigint; sequence?: bigint; protocolMajor?: number } = {},
): Envelope {
  return create(EnvelopeSchema, {
    protocolMajor: options.protocolMajor ?? PROTOCOL_MAJOR,
    protocolMinor: PROTOCOL_MINOR,
    requestId: options.requestId ?? 0n,
    sequence: options.sequence ?? 0n,
    streamId: 0n,
    priority: Priority.UNSPECIFIED,
    payload,
  });
}

/** A recorded-shape ServerHello, as apps/host/src/service.rs answers a compatible client. */
export function serverHello(overrides: MessageInitShape<typeof ServerHelloSchema> = {}): ServerHello {
  return create(ServerHelloSchema, {
    helperVersion: "0.1.0",
    helperBuildDigest: "a".repeat(64),
    operatingSystem: "linux",
    architecture: "x86_64",
    tmuxVersion: "tmux 3.7b",
    serverIdentity: "server-a",
    capabilities: HOST_CAPABILITIES,
    readOnly: false,
    incompatibility: "",
    gitVersion: "git version 2.50.0",
    connectionEpoch: 1n,
    terminalOutputWindowBytes: BigInt(2 * 1024 * 1024),
    terminalOutputWindowRecords: 1024,
    ...overrides,
  });
}

export function topologySnapshot(overrides: MessageInitShape<typeof SnapshotSchema> = {}): Snapshot {
  return create(SnapshotSchema, {
    serverIdentity: "server-a",
    generation: 1n,
    sessions: [{ id: "$1", name: "primary", windowCount: 1, attachedClients: 0, order: 0 }],
    windows: [{ id: "@1", sessionId: "$1", index: 0, name: "bash", active: true, layout: "", zoomed: false, layoutGeneration: 0n }],
    panes: [{ id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/home/u", currentCommand: "bash" }],
    ...overrides,
  });
}

export function okResponse(overrides: MessageInitShape<typeof ResponseSchema> = {}): Response {
  return create(ResponseSchema, { ok: true, ...overrides });
}

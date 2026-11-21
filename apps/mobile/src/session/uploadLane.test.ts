// The bulk lane an upload runs on (`openUploadConnection`): its own every
// time, bound to the control connection it was opened under, and gone with
// its upload. The shared lane the file viewer reads on is never touched.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { uploadFile, UploadError } from "../features/chat/attach/upload";
import { Operation, type Envelope } from "../protocol/gen/envelope_pb";
import { cancelTerminalUpload } from "../protocol/requests";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../protocol/testing/fakeTransport";
import type { SavedHost } from "../store/hostsStore";
import { connectHost, disconnectHost, getConnection, openBulkConnection, openUploadConnection, setConnectionAppActive, setTransportFactory } from "./connectionManager";

const host: SavedHost = { id: "h-upload", label: "Dev box", host: "dev.local", port: 22, user: "dev", trustedHostKeyFingerprint: null, connectionEpoch: 0, lastConnectedAtMs: null };
const settle = () => vi.advanceTimersByTimeAsync(0);
const UPLOADS = "/home/dev/.cache/muxflow/uploads";

describe("the upload's own bulk lane", () => {
  const dials: Array<{ lane: string; transport: FakeTransport }> = [];
  beforeEach(() => {
    setConnectionAppActive(true);
    vi.useFakeTimers();
    dials.length = 0;
    setTransportFactory(async (_host, lane) => {
      const transport = new FakeTransport();
      dials.push({ lane, transport });
      return transport;
    });
  });
  afterEach(async () => {
    await disconnectHost();
    setConnectionAppActive(true);
    vi.useRealTimers();
  });

  async function connectControl() {
    const pending = connectHost(host);
    await settle();
    const control = dials.at(-1)!.transport;
    const epoch = getConnection()!.connectionEpoch;
    control.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch }) }, { requestId: 1n }));
    control.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await pending;
    return { control, epoch };
  }

  /** Answers the lane's handshake, as the host does for a bulk connection on `epoch`. */
  function admit(transport: FakeTransport, epoch: bigint): void {
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch, terminalOutputWindowBytes: 0n }) }, { requestId: 1n }));
  }

  async function openUpload(epoch: bigint) {
    const pending = openUploadConnection();
    await settle();
    const dial = dials.at(-1)!;
    admit(dial.transport, epoch);
    return { lane: await pending, transport: dial.transport };
  }

  async function openShared(epoch: bigint) {
    const pending = openBulkConnection();
    await settle();
    const dial = dials.at(-1)!;
    admit(dial.transport, epoch);
    return { lane: await pending, transport: dial.transport };
  }

  it("dials a bulk lane bound to the control epoch and server, without Subscribe", async () => {
    const { epoch } = await connectControl();
    const pending = openUploadConnection();
    await settle();
    const dial = dials[1]!;
    expect(dial.lane).toBe("bulk");
    const [hello, ...rest] = dial.transport.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("expected ClientHello");
    expect(hello.payload.value).toMatchObject({ bulkConnection: true, expectedServerIdentity: "server-a", connectionEpoch: epoch });
    expect(rest).toHaveLength(0);
    admit(dial.transport, epoch);
    expect((await pending).state).toBe("connected");
  });

  it("is its own lane every time, and never the shared one", async () => {
    const { epoch } = await connectControl();
    const shared = await openShared(epoch);
    const first = await openUpload(epoch);
    const second = await openUpload(epoch);
    expect(dials.map((dial) => dial.lane)).toEqual(["control", "bulk", "bulk", "bulk"]);
    expect(first.lane).not.toBe(shared.lane);
    expect(second.lane).not.toBe(first.lane);

    // Closing an upload's lane leaves the others, and the shared lane is still the memoised one.
    first.lane.disconnect();
    expect(first.transport.closed).toBe(true);
    expect(second.transport.closed).toBe(false);
    expect(shared.transport.closed).toBe(false);
    expect(await openBulkConnection()).toBe(shared.lane);
    expect(dials).toHaveLength(4);

    // And re-dialling the shared lane does not close an upload's.
    shared.transport.closeFromRemote({ reason: "networkLost" });
    const again = await openShared(epoch);
    expect(again.lane).not.toBe(shared.lane);
    expect(second.transport.closed).toBe(false);
    expect(second.lane.state).toBe("connected");
  });

  it("is closed by its upload when the upload ends, with the shared lane left open", async () => {
    const { epoch } = await connectControl();
    const shared = await openShared(epoch);
    const bytes = Uint8Array.from({ length: 3000 }, (_, index) => index % 251);
    const upload = uploadFile({
      openLane: openUploadConnection,
      source: { name: "shot.png", size: bytes.length, read: async (offset, length) => bytes.subarray(offset, offset + length) },
      signal: new AbortController().signal,
    });
    await settle();
    const transport = dials.at(-1)!.transport;
    expect(transport).not.toBe(shared.transport);
    transport.drain();
    admit(transport, epoch);

    // The host's side of the upload, answered as each request arrives.
    const seen: Operation[] = [];
    const answer = (envelope: Envelope): void => {
      if (envelope.payload.case !== "request") return;
      const request = envelope.payload.value;
      const file = request.file!;
      seen.push(request.operation);
      const upload = { transferId: file.transferId, destinationName: "shot.png", totalBytes: file.totalBytes };
      const value = request.operation === Operation.WRITE_TERMINAL_UPLOAD_CHUNK
        ? okResponse({ file: { transferChunk: { transferId: file.transferId, offset: file.offset + BigInt(file.content.length), totalBytes: file.totalBytes, totalKnown: true } } })
        : request.operation === Operation.COMMIT_TERMINAL_UPLOAD
          ? okResponse({ file: { upload: { ...upload, finalPath: `${UPLOADS}/shot.png`, verified: true, blake3: file.blake3 } } })
          : okResponse({ file: { upload } });
      transport.feed(hostEnvelope({ case: "response", value }, { requestId: envelope.requestId }));
    };
    for (let turn = 0; turn < 12 && !transport.closed; turn += 1) {
      await settle();
      transport.drain().forEach(answer);
    }

    expect(await upload).toEqual({ hostPath: `${UPLOADS}/shot.png`, name: "shot.png", sizeBytes: 3000 });
    expect(seen).toEqual([Operation.PREPARE_TERMINAL_UPLOAD, Operation.WRITE_TERMINAL_UPLOAD_CHUNK, Operation.COMMIT_TERMINAL_UPLOAD]);
    expect(transport.closed).toBe(true);
    expect(shared.transport.closed).toBe(false);
    expect(shared.lane.state).toBe("connected");
    // The next upload gets a lane of its own.
    const next = await openUpload(epoch);
    expect(next.transport).not.toBe(transport);
  });

  it("goes when the control connection reconnects on a new epoch: the upload on it fails", async () => {
    const { control, epoch } = await connectControl();
    const upload = await openUpload(epoch);
    // Any request: it only has to be in flight when the lane goes.
    const request = upload.lane.request(cancelTerminalUpload("00000000-0000-4000-8000-000000000000")).catch((error: Error) => error);
    control.closeFromRemote({ reason: "networkLost" });
    await vi.advanceTimersByTimeAsync(1_000);
    const control2 = dials.at(-1)!.transport;
    const epoch2 = getConnection()!.connectionEpoch;
    expect(epoch2).toBe(epoch + 1n);
    control2.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch2 }) }, { requestId: 1n }));
    control2.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await settle();
    expect(upload.transport.closed).toBe(true);
    expect(await request).toMatchObject({ name: "ConnectionClosedError" });

    // A lane opened now is bound to the new epoch.
    const pending = openUploadConnection();
    await settle();
    const [hello] = dials.at(-1)!.transport.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("expected ClientHello");
    expect(hello.payload.value.connectionEpoch).toBe(epoch2);
    admit(dials.at(-1)!.transport, epoch2);
    await pending;
  });

  it("goes when the phone disconnects, open or still being dialled", async () => {
    const { epoch } = await connectControl();
    const open = await openUpload(epoch);
    const dialling = openUploadConnection().catch((error: Error) => error);
    await settle();
    const late = dials.at(-1)!.transport;
    await disconnectHost();
    expect(open.transport.closed).toBe(true);
    expect(late.closed).toBe(true);
    expect(await dialling).toBeInstanceOf(Error);
  });

  it("is not dialled again when it drops: an upload does not outlive its connection", async () => {
    const { epoch } = await connectControl();
    const upload = await openUpload(epoch);
    const before = dials.length;
    upload.transport.closeFromRemote({ reason: "networkLost" });
    expect(upload.lane.state).toBe("idle");
    await vi.advanceTimersByTimeAsync(120_000);
    expect(dials).toHaveLength(before);
    // The control connection is as it was.
    expect(getConnection()!.state).toBe("connected");
  });

  it("says why when the host refuses the lane, and dials nothing more for it", async () => {
    const { epoch } = await connectControl();
    const pending = openUploadConnection().catch((error: Error) => error);
    await settle();
    const dial = dials.at(-1)!;
    // Admitted under another epoch: not the control connection this lane was bound to.
    admit(dial.transport, epoch + 5n);
    const refused = await pending;
    expect(refused).toBeInstanceOf(Error);
    expect((refused as Error).message).toBe("bulk connection was bound to a different control connection");
    const before = dials.length;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(dials).toHaveLength(before);
  });

  it("is refused while the phone is not connected, and the upload says so", async () => {
    await expect(openUploadConnection()).rejects.toThrow("not connected");
    const upload = uploadFile({
      openLane: openUploadConnection,
      source: { name: "a.txt", size: 1, read: async () => new Uint8Array(1) },
      signal: new AbortController().signal,
    });
    await expect(upload).rejects.toBeInstanceOf(UploadError);
    await expect(upload).rejects.toThrow("not connected");
    expect(dials).toEqual([]);
  });
});

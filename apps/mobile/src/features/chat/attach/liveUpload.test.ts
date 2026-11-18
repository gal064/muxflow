// Attachments against the real `muxflow-host bridge --stdio`
// (docs/design/mobile-chat-plan.md §3 "Attachments"). Skipped when cargo or
// tmux is missing. Every observation is logged with a `live:` prefix so the
// run's output is the evidence.
//
// The host's upload folder is `$HOME/.cache/muxflow/uploads`, so the helper
// runs with HOME pointing at a scratch directory (`isolateHome`): nothing here
// writes under the home of whoever runs the test, and the test checks that.

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { liveHostAvailability, startHostHarness, type HostHarness } from "../../../../scripts/host-harness";
import { HostConnection } from "../../../protocol/HostConnection";
import { createSessionStore } from "../../../store/sessionStore";
import { AttachmentQueue } from "./attachmentQueue";
import { readAttachedImage, type AttachmentPane } from "./readImage";
import { UploadError, uploadFile, type ByteSource, type UploadLane } from "./upload";

const availability = liveHostAvailability();
const MIB = 1024 * 1024;

/** A whole 1x1 PNG. */
const PNG = Uint8Array.from(Buffer.from(
  "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63304e9bf91f00043402325b2d0ae00000000049454e44ae426082",
  "hex",
));

function bytesOf(length: number): Uint8Array {
  return Uint8Array.from({ length }, (_, index) => (index * 31 + (index >>> 8)) % 256);
}

function sourceOf(name: string, bytes: Uint8Array): ByteSource {
  return { name, size: bytes.length, read: async (offset, length) => bytes.subarray(offset, offset + length) };
}

async function waitFor<T>(label: string, probe: () => T | undefined, timeoutMs = 15_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

describe.skipIf(!availability.available)(`live upload (${availability.reason ?? "cargo + tmux present"})`, () => {
  let harness: HostHarness;
  const store = createSessionStore();
  let control: HostConnection;
  let epoch = 0;
  const lanes: HostConnection[] = [];
  /** In every file name, so a stray file would be recognised anywhere. */
  const tag = `m6live-${process.pid}-${Date.now().toString(36)}`;
  const say = (line: string) => console.log(`live: ${line}`);

  /** A bulk lane of its own, as `openBulkConnection` gives the app: a second bridge bound to the control connection. */
  const openLane = async (): Promise<HostConnection> => {
    const laneStore = createSessionStore();
    const lane = new HostConnection({
      dial: async () => harness.dial(),
      nextConnectionEpoch: () => {
        throw new Error("a bulk lane reuses the control connection's epoch");
      },
      store: laneStore,
      bulk: { expectedServerIdentity: control.serverIdentity, connectionEpoch: control.connectionEpoch },
    });
    lanes.push(lane);
    lane.connect();
    await waitFor("bulk connected", () => (laneStore.getState().connection.state === "connected" ? true : undefined));
    return lane;
  };

  beforeAll(async () => {
    harness = await startHostHarness({ isolateHome: true });
    control = new HostConnection({
      dial: async () => harness.transport,
      nextConnectionEpoch: () => (epoch += 1),
      store,
    });
  }, 600_000);

  afterAll(async () => {
    for (const lane of lanes) lane.disconnect();
    control?.disconnect();
    await harness?.stop();
  }, 60_000);

  it("uploads files into the host's upload folder, renames a taken name, cancels cleanly, and reads an image back", async () => {
    const home = harness.home!;
    const uploads = path.join(home, ".cache", "muxflow", "uploads");
    expect(home).not.toBe(homedir());
    expect(home.startsWith(`${homedir()}/`)).toBe(false);
    say(`helper HOME=${home} (the user's is ${homedir()})`);

    control.connect();
    await waitFor("connected", () => (store.getState().connection.state === "connected" ? true : undefined));
    say(`control connected: identity=${control.serverIdentity}`);

    const signal = new AbortController().signal;
    const upload = (name: string, bytes: Uint8Array, extra: { signal?: AbortSignal; onProgress?: (sent: number, total: number) => void } = {}) => {
      let lane: HostConnection | undefined;
      const result = uploadFile({
        openLane: async (): Promise<UploadLane> => (lane = await openLane()),
        source: sourceOf(name, bytes),
        signal,
        ...extra,
      });
      return { result, lane: () => lane };
    };

    // A file of two and a half chunks.
    const trace = bytesOf(2 * MIB + MIB / 2);
    const traceName = `${tag}-trace.bin`;
    const progress: number[] = [];
    const first = upload(traceName, trace, { onProgress: (sent) => progress.push(sent) });
    const traced = await first.result;
    say(`upload ${traceName} (${trace.length} B) → ${traced.hostPath}, progress ${progress.join(" → ")}`);
    expect(traced).toEqual({ hostPath: path.join(uploads, traceName), name: traceName, sizeBytes: trace.length });
    expect(progress).toEqual([MIB, 2 * MIB, trace.length]);
    expect(new Uint8Array(readFileSync(traced.hostPath))).toEqual(trace);
    say(`${traced.hostPath} holds the same ${trace.length} bytes`);
    // The upload closed the connection it opened.
    expect(first.lane()!.state).toBe("idle");

    // A tiny PNG, then the same name again.
    const pngName = `${tag}-pixel.png`;
    const png = await upload(pngName, PNG).result;
    say(`upload ${pngName} (${PNG.length} B) → ${png.hostPath}`);
    expect(png.hostPath).toBe(path.join(uploads, pngName));
    expect(new Uint8Array(readFileSync(png.hostPath))).toEqual(PNG);

    const again = await upload(pngName, PNG).result;
    say(`upload ${pngName} again → ${again.hostPath} (name ${again.name})`);
    expect(again.name).toBe(`${tag}-pixel (1).png`);
    expect(again.hostPath).toBe(path.join(uploads, `${tag}-pixel (1).png`));
    expect(new Uint8Array(readFileSync(again.hostPath))).toEqual(PNG);
    expect(existsSync(png.hostPath)).toBe(true);

    // Cancelled after its first chunk: no file by that name, and no partial.
    const abort = new AbortController();
    const cutName = `${tag}-cut.bin`;
    const sent: number[] = [];
    let partialSeen: string[] = [];
    const cut = upload(cutName, bytesOf(4 * MIB), {
      signal: abort.signal,
      onProgress: (bytes) => {
        sent.push(bytes);
        if (sent.length === 1) {
          partialSeen = readdirSync(uploads).filter((name) => name.endsWith(".partial"));
          abort.abort();
        }
      },
    });
    const cancelled = await cut.result.then(() => undefined, (error: unknown) => error);
    expect(cancelled).toBeInstanceOf(UploadError);
    expect((cancelled as UploadError).aborted).toBe(true);
    const afterCancel = readdirSync(uploads);
    say(`upload ${cutName} cancelled after ${sent.join(", ")} B; partial while it ran: ${partialSeen.join(", ") || "none"}`);
    say(`upload folder after the cancel: ${afterCancel.join(" | ")}`);
    expect(partialSeen).toHaveLength(1);
    expect(afterCancel.filter((name) => name.includes("cut"))).toEqual([]);
    expect(afterCancel.filter((name) => name.endsWith(".partial") || name.includes("reservation"))).toEqual([]);
    expect(cut.lane()!.state).toBe("idle");

    // The queue on the real uploader: two files, one at a time, then ready.
    const queue = new AttachmentQueue((source, uploadSignal, onProgress) => uploadFile({ openLane, source, signal: uploadSignal, onProgress }));
    queue.add(sourceOf(`${tag}-queued.txt`, new TextEncoder().encode("from the queue\n")));
    queue.add(sourceOf(pngName, PNG));
    await waitFor("queue ready", () => (queue.ready() ? true : undefined));
    const forSend = queue.attachmentsForSend();
    say(`queue ready → ${forSend.map((attachment) => `${attachment.name} at ${attachment.path}`).join(" ; ")}`);
    expect(forSend.map((attachment) => attachment.name)).toEqual([`${tag}-queued.txt`, pngName]);
    expect(forSend[1]!.path).toBe(path.join(uploads, `${tag}-pixel (2).png`));
    expect(readFileSync(forSend[0]!.path, "utf8")).toBe("from the queue\n");

    // Reading the PNG back, as tapping its chip would.
    const state = store.getState();
    const livePane = Object.values(state.panes).find((pane) => pane.sessionId === harness.primarySessionId)!;
    const pane: AttachmentPane = {
      pane: { id: livePane.id, sessionId: livePane.sessionId, windowId: livePane.windowId, currentPath: livePane.currentPath },
      serverIdentity: control.serverIdentity,
      topologyGeneration: state.topologyGeneration,
    };
    const reader = { control, openBulk: openLane };
    const image = await readAttachedImage(reader, pane, png.hostPath);
    say(`read back ${png.hostPath} from pane ${livePane.id} (cwd ${livePane.currentPath}) → ${image.bytes.length} B, ${image.mediaType}, name ${image.name}`);
    expect(image).toEqual({ bytes: PNG, mediaType: "image/png", name: pngName });

    const notImage = await readAttachedImage(reader, pane, traced.hostPath).then(() => "read", (error: unknown) => (error as Error).message);
    say(`read back ${traceName} → ${notImage}`);
    expect(notImage).toBe("That attachment is not an image.");

    // Everything went under the scratch HOME, and nothing under the real one.
    const final = readdirSync(uploads).sort();
    say(`scratch HOME holds: ${readdirSync(home).join(", ")}; uploads: ${final.filter((name) => !name.startsWith(".")).join(" | ")}`);
    expect(final.filter((name) => !name.startsWith("."))).toEqual([
      `${tag}-pixel (1).png`,
      `${tag}-pixel (2).png`,
      `${tag}-pixel.png`,
      `${tag}-queued.txt`,
      `${tag}-trace.bin`,
    ].sort());
    const realUploads = path.join(homedir(), ".cache", "muxflow", "uploads");
    const stray = existsSync(realUploads) ? readdirSync(realUploads).filter((name) => name.includes(tag)) : [];
    say(`files of this run under ${realUploads}: ${stray.length}`);
    expect(stray).toEqual([]);
  }, 120_000);
});

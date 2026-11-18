import { create } from "@bufbuild/protobuf";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { describe, expect, it } from "vitest";
import {
  CollisionPolicy,
  FileServiceResponseSchema,
  Operation,
  ResponseSchema,
  TransferChunkSchema,
  UploadDescriptorSchema,
  type Request,
  type Response,
} from "../../../protocol/gen/envelope_pb";
import { ConnectionClosedError, HostError, REQUEST_TIMEOUT_MS, RequestTimeoutError } from "../../../protocol/HostConnection";
import { mediaTypeForName } from "./mediaType";
import {
  CANCEL_TIMEOUT_MS,
  CHUNK_TIMEOUT_MS,
  COMMIT_TIMEOUT_MS,
  MAX_IMAGE_UPLOAD_BYTES,
  MAX_UPLOAD_BYTES,
  newTransferId,
  UploadError,
  uploadFile,
  uploadRefusal,
  type ByteSource,
  type UploadLane,
} from "./upload";

const MIB = 1024 * 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;

function bytesOf(length: number): Uint8Array {
  return Uint8Array.from({ length }, (_, index) => (index * 31 + (index >>> 8)) % 256);
}

function sourceOf(name: string, bytes: Uint8Array): ByteSource {
  return { name, size: bytes.length, read: async (offset, length) => bytes.subarray(offset, offset + length) };
}

/** A source of a given size that must never be read. */
function sized(name: string, size: number): ByteSource {
  return { name, size, read: async () => { throw new Error("read"); } };
}

function ok(file: Parameters<typeof create<typeof FileServiceResponseSchema>>[1]): Response {
  return create(ResponseSchema, { ok: true, file: create(FileServiceResponseSchema, file) });
}

interface Sent {
  operation: Operation;
  request: Request;
  timeoutMs: number | undefined;
}

/**
 * The host's side of an upload, as terminal_upload.rs keeps it: one partial
 * per transfer, chunks in order, a digest of what arrived. `answer` lets a
 * test replace one answer.
 */
class FakeLane implements UploadLane {
  readonly sent: Sent[] = [];
  readonly partials = new Map<string, { name: string; total: bigint; bytes: number[] }>();
  readonly published = new Map<string, Uint8Array>();
  disconnects = 0;
  answer: ((sent: Sent, honest: () => Response) => Promise<Response> | Response) | undefined;

  request(request: Request, options?: { timeoutMs?: number }): Promise<Response> {
    const sent = { operation: request.operation, request, timeoutMs: options?.timeoutMs };
    this.sent.push(sent);
    const honest = (): Response => this.honest(request);
    return Promise.resolve().then(() => (this.answer ? this.answer(sent, honest) : honest()));
  }

  disconnect(): void {
    this.disconnects += 1;
    this.partials.clear();
  }

  operations(): Operation[] {
    return this.sent.map((sent) => sent.operation);
  }

  private honest(request: Request): Response {
    const file = request.file!;
    const id = file.transferId;
    switch (request.operation) {
      case Operation.PREPARE_TERMINAL_UPLOAD: {
        this.partials.set(id, { name: file.destination, total: file.totalBytes, bytes: [] });
        return ok({ upload: create(UploadDescriptorSchema, { transferId: id, destinationName: file.destination, totalBytes: file.totalBytes }) });
      }
      case Operation.WRITE_TERMINAL_UPLOAD_CHUNK: {
        const partial = this.partials.get(id);
        if (!partial) throw new HostError("upload_chunk_rejected", "terminal upload is not active");
        if (file.content.length === 0 || file.content.length > MIB) {
          throw new HostError("upload_chunk_rejected", "terminal-upload chunks must contain 1 byte through 1 MiB");
        }
        if (file.offset !== BigInt(partial.bytes.length)) {
          throw new HostError("upload_chunk_rejected", `stale terminal-upload offset: expected ${partial.bytes.length}, received ${file.offset}`);
        }
        for (const byte of file.content) partial.bytes.push(byte);
        return ok({ transferChunk: create(TransferChunkSchema, { transferId: id, offset: BigInt(partial.bytes.length), totalBytes: file.totalBytes, totalKnown: true }) });
      }
      case Operation.COMMIT_TERMINAL_UPLOAD: {
        const partial = this.partials.get(id);
        if (!partial) throw new HostError("upload_commit_not_published", "terminal upload is not active");
        this.partials.delete(id);
        const bytes = Uint8Array.from(partial.bytes);
        const digest = bytesToHex(blake3(bytes));
        if (BigInt(bytes.length) !== partial.total || digest !== file.blake3) {
          throw new HostError("upload_commit_not_published", "terminal-upload BLAKE3 verification failed");
        }
        const path = `/home/dev/.cache/muxflow/uploads/${partial.name}`;
        this.published.set(path, bytes);
        return ok({
          upload: create(UploadDescriptorSchema, {
            transferId: id,
            destinationName: partial.name,
            totalBytes: partial.total,
            finalPath: path,
            verified: true,
            blake3: digest,
          }),
        });
      }
      case Operation.CANCEL_TERMINAL_UPLOAD:
        this.partials.delete(id);
        return ok({ upload: create(UploadDescriptorSchema, { transferId: id }) });
      default:
        throw new HostError("unknown", "unexpected operation");
    }
  }
}

function run(lane: FakeLane, source: ByteSource, extra: Partial<Parameters<typeof uploadFile>[0]> = {}) {
  return uploadFile({ openLane: async () => lane, source, signal: new AbortController().signal, ...extra });
}

async function failure(upload: Promise<unknown>): Promise<UploadError> {
  const error = await upload.then(() => undefined, (reason: unknown) => reason);
  expect(error).toBeInstanceOf(UploadError);
  return error as UploadError;
}

describe("BLAKE3 from @noble/hashes", () => {
  it("matches the reference digest of the empty input", () => {
    expect(bytesToHex(blake3(new Uint8Array(0)))).toBe("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
  });

  it("hashes in pieces to the same digest as at once", () => {
    const bytes = bytesOf(3 * MIB + 17);
    const hasher = blake3.create();
    for (let offset = 0; offset < bytes.length; offset += MIB) hasher.update(bytes.subarray(offset, offset + MIB));
    expect(bytesToHex(hasher.digest())).toBe(bytesToHex(blake3(bytes)));
  });
});

describe("what the phone will not send", () => {
  it("refuses an image over 25 MiB in the desktop's words, and any file over 100 MiB", () => {
    expect(uploadRefusal("shot.png", MAX_IMAGE_UPLOAD_BYTES)).toBeUndefined();
    expect(uploadRefusal("shot.PNG", MAX_IMAGE_UPLOAD_BYTES + 1)).toBe("shot.PNG is over 25 MB, the most an image can be.");
    expect(uploadRefusal("clip.heic", MAX_IMAGE_UPLOAD_BYTES + 1)).toBe("clip.heic is over 25 MB, the most an image can be.");
    expect(uploadRefusal("build.log", MAX_IMAGE_UPLOAD_BYTES + 1)).toBeUndefined();
    expect(uploadRefusal("build.log", MAX_UPLOAD_BYTES)).toBeUndefined();
    expect(uploadRefusal("build.log", MAX_UPLOAD_BYTES + 1)).toBe("That file is too large to send from the phone.");
    expect(uploadRefusal("shot.png", MAX_UPLOAD_BYTES + 1)).toBe("That file is too large to send from the phone.");
  });

  it("refuses before a connection is opened", async () => {
    let opened = 0;
    const openLane = async (): Promise<UploadLane> => {
      opened += 1;
      return new FakeLane();
    };
    const signal = new AbortController().signal;
    const tooLarge = await failure(uploadFile({ openLane, source: sized("video.mov", MAX_UPLOAD_BYTES + 1), signal }));
    expect(tooLarge.message).toBe("That file is too large to send from the phone.");
    const image = await failure(uploadFile({ openLane, source: sized("photo.jpg", MAX_IMAGE_UPLOAD_BYTES + 1), signal }));
    expect(image.message).toBe("photo.jpg is over 25 MB, the most an image can be.");
    expect(opened).toBe(0);
  });

  it("knows an image by its extension", () => {
    expect(["a.png", "a.jpg", "a.JPEG", "a.gif", "a.webp", "IMG_1.HEIC"].map(mediaTypeForName)).toEqual([
      "image/png", "image/jpeg", "image/jpeg", "image/gif", "image/webp", "image/heic",
    ]);
    expect(["notes.txt", "png", "archive.tar.gz", ""].map(mediaTypeForName)).toEqual(["", "", "", ""]);
  });
});

describe("uploading a file", () => {
  it("prepares, writes chunks of at most 1 MiB, commits with the digest, and closes its connection", async () => {
    const lane = new FakeLane();
    const bytes = bytesOf(2 * MIB + MIB / 2);
    const progress: Array<[number, number]> = [];
    const uploaded = await run(lane, sourceOf("trace.bin", bytes), { onProgress: (sent, total) => progress.push([sent, total]) });

    expect(uploaded).toEqual({ hostPath: "/home/dev/.cache/muxflow/uploads/trace.bin", name: "trace.bin", sizeBytes: bytes.length });
    expect(lane.published.get(uploaded.hostPath)).toEqual(bytes);
    expect(lane.operations()).toEqual([
      Operation.PREPARE_TERMINAL_UPLOAD,
      Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
      Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
      Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
      Operation.COMMIT_TERMINAL_UPLOAD,
    ]);
    expect(progress).toEqual([[MIB, bytes.length], [2 * MIB, bytes.length], [bytes.length, bytes.length]]);
    expect(lane.disconnects).toBe(1);

    const [prepare, first, second, third, commit] = lane.sent.map((sent) => sent.request.file!);
    const id = prepare!.transferId;
    expect(id).toMatch(UUID);
    expect(prepare).toMatchObject({ operationId: id, destination: "trace.bin", sourceName: "trace.bin", totalBytes: BigInt(bytes.length), collisionPolicy: CollisionPolicy.RENAME });
    expect([first, second, third].map((chunk) => [chunk!.transferId, chunk!.offset, chunk!.content.length])).toEqual([
      [id, 0n, MIB], [id, BigInt(MIB), MIB], [id, BigInt(2 * MIB), MIB / 2],
    ]);
    expect(commit).toMatchObject({ operationId: id, transferId: id, totalBytes: BigInt(bytes.length), blake3: bytesToHex(blake3(bytes)) });
  });

  it("gives a slow link more time than the connection's default", async () => {
    const lane = new FakeLane();
    await run(lane, sourceOf("a.txt", bytesOf(10)));
    const timeouts = new Map(lane.sent.map((sent) => [sent.operation, sent.timeoutMs]));
    expect(timeouts.get(Operation.WRITE_TERMINAL_UPLOAD_CHUNK)).toBe(CHUNK_TIMEOUT_MS);
    expect(timeouts.get(Operation.COMMIT_TERMINAL_UPLOAD)).toBe(COMMIT_TIMEOUT_MS);
    expect(Math.min(CHUNK_TIMEOUT_MS, COMMIT_TIMEOUT_MS)).toBeGreaterThan(REQUEST_TIMEOUT_MS);
  });

  it("sends an empty file as a prepare and a commit", async () => {
    const lane = new FakeLane();
    const uploaded = await run(lane, sourceOf("empty.txt", new Uint8Array(0)));
    expect(lane.operations()).toEqual([Operation.PREPARE_TERMINAL_UPLOAD, Operation.COMMIT_TERMINAL_UPLOAD]);
    expect(lane.sent[1]!.request.file!.blake3).toBe("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
    expect(uploaded.sizeBytes).toBe(0);
  });

  it("answers the name the host gave the file when its own was taken", async () => {
    const lane = new FakeLane();
    lane.answer = (sent, honest) => {
      if (sent.operation !== Operation.COMMIT_TERMINAL_UPLOAD) return honest();
      const upload = honest().file!.upload!;
      return ok({ upload: create(UploadDescriptorSchema, { ...upload, destinationName: "shot (1).png", finalPath: "/home/dev/.cache/muxflow/uploads/shot (1).png" }) });
    };
    const uploaded = await run(lane, sourceOf("shot.png", bytesOf(64)));
    expect(uploaded).toEqual({ hostPath: "/home/dev/.cache/muxflow/uploads/shot (1).png", name: "shot (1).png", sizeBytes: 64 });
  });

  it("makes transfer ids the host accepts", () => {
    const ids = Array.from({ length: 50 }, () => newTransferId());
    for (const id of ids) expect(id).toMatch(UUID);
    expect(new Set(ids).size).toBe(ids.length);
  });
});

describe("an upload that fails", () => {
  it("says what the host said, cancels, and closes its connection", async () => {
    const lane = new FakeLane();
    lane.answer = (sent, honest) => {
      if (sent.operation === Operation.PREPARE_TERMINAL_UPLOAD) {
        throw new HostError("upload_preflight_rejected", "insufficient destination space: need 10 bytes, have 0");
      }
      return honest();
    };
    const error = await failure(run(lane, sourceOf("a.txt", bytesOf(10))));
    expect(error.message).toBe("insufficient destination space: need 10 bytes, have 0");
    expect(error.aborted).toBe(false);
    expect(lane.operations()).toEqual([Operation.PREPARE_TERMINAL_UPLOAD, Operation.CANCEL_TERMINAL_UPLOAD]);
    expect(lane.sent[1]!.timeoutMs).toBe(CANCEL_TIMEOUT_MS);
    expect(lane.sent[1]!.request.file!.transferId).toBe(lane.sent[0]!.request.file!.transferId);
    expect(lane.disconnects).toBe(1);
  });

  it("stops at an acknowledgement for the wrong offset", async () => {
    const lane = new FakeLane();
    lane.answer = (sent, honest) => {
      const answer = honest();
      if (sent.operation !== Operation.WRITE_TERMINAL_UPLOAD_CHUNK || sent.request.file!.offset === 0n) return answer;
      return ok({ transferChunk: create(TransferChunkSchema, { offset: 7n }) });
    };
    const error = await failure(run(lane, sourceOf("a.bin", bytesOf(3 * MIB))));
    expect(error.message).toBe("The host lost track of the upload.");
    expect(lane.operations()).toEqual([
      Operation.PREPARE_TERMINAL_UPLOAD,
      Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
      Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
      Operation.CANCEL_TERMINAL_UPLOAD,
    ]);
    expect(lane.published.size).toBe(0);
    expect(lane.disconnects).toBe(1);
  });

  it("does not take a commit the host did not verify, or verified for other bytes", async () => {
    for (const change of [{ verified: false }, { blake3: "00" }, { totalBytes: 9n }, { finalPath: "" }]) {
      const lane = new FakeLane();
      lane.answer = (sent, honest) => {
        if (sent.operation !== Operation.COMMIT_TERMINAL_UPLOAD) return honest();
        return ok({ upload: create(UploadDescriptorSchema, { ...honest().file!.upload!, ...change }) });
      };
      const error = await failure(run(lane, sourceOf("a.txt", bytesOf(10))));
      expect(error.message).toBe("The host could not verify the upload.");
      expect(lane.disconnects).toBe(1);
    }
  });

  it("fails when the commit answer is lost, without asking what became of it", async () => {
    const lane = new FakeLane();
    lane.answer = (sent, honest) => {
      if (sent.operation === Operation.COMMIT_TERMINAL_UPLOAD) throw new RequestTimeoutError(9n);
      return honest();
    };
    const error = await failure(run(lane, sourceOf("a.txt", bytesOf(10))));
    expect(error.message).toBe("The host didn't answer in time.");
    expect(lane.operations()).not.toContain(Operation.RECONCILE_TERMINAL_UPLOAD);
    expect(lane.operations().at(-1)).toBe(Operation.CANCEL_TERMINAL_UPLOAD);
    expect(lane.disconnects).toBe(1);
  });

  it("sends no cancel down a connection that is gone, and still closes it", async () => {
    const lane = new FakeLane();
    lane.answer = (sent, honest) => {
      if (sent.operation === Operation.WRITE_TERMINAL_UPLOAD_CHUNK) throw new ConnectionClosedError("networkLost");
      return honest();
    };
    const error = await failure(run(lane, sourceOf("a.txt", bytesOf(10))));
    expect(error.message).toBe("The connection to the host was lost.");
    expect(lane.operations()).toEqual([Operation.PREPARE_TERMINAL_UPLOAD, Operation.WRITE_TERMINAL_UPLOAD_CHUNK]);
    expect(lane.disconnects).toBe(1);
  });

  it("fails when the file is shorter than it said", async () => {
    const lane = new FakeLane();
    const bytes = bytesOf(100);
    const source: ByteSource = { name: "a.txt", size: 200, read: async (offset, length) => bytes.subarray(offset, offset + length) };
    const error = await failure(run(lane, source));
    expect(error.message).toBe("The file changed while it was being sent.");
    expect(lane.operations()).toEqual([Operation.PREPARE_TERMINAL_UPLOAD, Operation.CANCEL_TERMINAL_UPLOAD]);
  });

  it("says why the connection could not be opened", async () => {
    const error = await failure(uploadFile({
      openLane: async () => { throw new Error("not connected"); },
      source: sourceOf("a.txt", bytesOf(10)),
      signal: new AbortController().signal,
    }));
    expect(error.message).toBe("not connected");
  });
});

describe("an upload that is cancelled", () => {
  it("stops mid-way, cancels on the host, and closes its connection", async () => {
    const lane = new FakeLane();
    const abort = new AbortController();
    const upload = run(lane, sourceOf("a.bin", bytesOf(4 * MIB)), {
      signal: abort.signal,
      onProgress: (sent) => {
        if (sent === 2 * MIB) abort.abort();
      },
    });
    const error = await failure(upload);
    expect(error.aborted).toBe(true);
    expect(lane.operations().filter((operation) => operation === Operation.WRITE_TERMINAL_UPLOAD_CHUNK).length).toBeLessThanOrEqual(3);
    expect(lane.operations().at(-1)).toBe(Operation.CANCEL_TERMINAL_UPLOAD);
    expect(lane.operations()).not.toContain(Operation.COMMIT_TERMINAL_UPLOAD);
    expect(lane.partials.size).toBe(0);
    expect(lane.published.size).toBe(0);
    expect(lane.disconnects).toBe(1);
  });

  it("does not wait for a chunk that is still on its way", async () => {
    const lane = new FakeLane();
    const abort = new AbortController();
    lane.answer = (sent, honest) => {
      if (sent.operation !== Operation.WRITE_TERMINAL_UPLOAD_CHUNK) return honest();
      // Never answered: only the abort ends it.
      queueMicrotask(() => abort.abort());
      return new Promise<Response>(() => undefined);
    };
    const error = await failure(run(lane, sourceOf("a.bin", bytesOf(10)), { signal: abort.signal }));
    expect(error.aborted).toBe(true);
    expect(lane.operations()).toEqual([
      Operation.PREPARE_TERMINAL_UPLOAD,
      Operation.WRITE_TERMINAL_UPLOAD_CHUNK,
      Operation.CANCEL_TERMINAL_UPLOAD,
    ]);
    expect(lane.disconnects).toBe(1);
  });

  it("opens nothing when it was cancelled before it began", async () => {
    let opened = 0;
    const abort = new AbortController();
    abort.abort();
    const error = await failure(uploadFile({
      openLane: async () => {
        opened += 1;
        return new FakeLane();
      },
      source: sourceOf("a.txt", bytesOf(10)),
      signal: abort.signal,
    }));
    expect(error.aborted).toBe(true);
    expect(opened).toBe(0);
  });

  it("closes a connection that arrives after the cancel", async () => {
    const lane = new FakeLane();
    const abort = new AbortController();
    let arrive: (lane: UploadLane) => void = () => undefined;
    const upload = uploadFile({
      openLane: () => new Promise<UploadLane>((resolve) => { arrive = resolve; }),
      source: sourceOf("a.txt", bytesOf(10)),
      signal: abort.signal,
    });
    abort.abort();
    expect((await failure(upload)).aborted).toBe(true);
    arrive(lane);
    await Promise.resolve();
    await Promise.resolve();
    expect(lane.sent).toEqual([]);
    expect(lane.disconnects).toBe(1);
  });
});

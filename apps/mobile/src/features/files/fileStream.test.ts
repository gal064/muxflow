import { describe, expect, it, vi } from "vitest";
import { create } from "@bufbuild/protobuf";
import { FileContentKind, ResponseSchema, type FileStreamFrame, type Request, type Response } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import { FileStreamAssembler, FileStreamError, MAX_CLIENT_FILE_BYTES, readFile } from "./fileStream";
import { bodyFrame, headerFrame } from "./testing";

const OPERATION = "op-1";
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function textHeader(totalBytes: number): FileStreamFrame {
  return headerFrame(OPERATION, {
    contentKind: FileContentKind.TEXT,
    totalBytes: BigInt(totalBytes),
    contentStreaming: true,
  });
}

describe("assembling a body (§9.7 step 1, §11.1)", () => {
  it("joins multiple frames by offset and answers with the whole file", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    const text = "# hello\n\nfrom the phone\n";
    const bytes = encoder.encode(text);
    assembler.push(textHeader(bytes.length));
    assembler.push(bodyFrame(OPERATION, 0, bytes.slice(0, 8)));
    assembler.push(bodyFrame(OPERATION, 8, bytes.slice(8), true));
    const body = assembler.finish();
    expect(body.kind).toBe("text");
    expect(decoder.decode((body as { bytes: Uint8Array }).bytes)).toBe(text);
  });

  it("refuses a frame that skips ahead, which would leave a zero-filled hole", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    const bytes = encoder.encode("abcdef");
    assembler.push(textHeader(6));
    expect(() => assembler.push(bodyFrame(OPERATION, 3, bytes.slice(3), true))).toThrow(FileStreamError);
  });

  it("refuses a frame that repeats bytes already written", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(6));
    assembler.push(bodyFrame(OPERATION, 0, encoder.encode("abc")));
    expect(() => assembler.push(bodyFrame(OPERATION, 0, encoder.encode("abc")))).toThrow(FileStreamError);
  });

  it("ignores frames belonging to another operation", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(3));
    assembler.push(bodyFrame("op-other", 0, encoder.encode("XXX"), true));
    assembler.push(bodyFrame(OPERATION, 0, encoder.encode("abc"), true));
    expect(decoder.decode((assembler.finish() as { bytes: Uint8Array }).bytes)).toBe("abc");
  });

  it("refuses a body that ends before the declared length", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(10));
    assembler.push(bodyFrame(OPERATION, 0, encoder.encode("abc"), true));
    expect(() => assembler.finish()).toThrow(FileStreamError);
  });

  it("refuses a body that never sent its eof frame", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(3));
    assembler.push(bodyFrame(OPERATION, 0, encoder.encode("abc")));
    expect(() => assembler.finish()).toThrow(/eof/u);
  });

  it("refuses a frame that would run past the declared length", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(3));
    expect(() => assembler.push(bodyFrame(OPERATION, 0, encoder.encode("abcd"), true))).toThrow(FileStreamError);
  });

  it("refuses a body frame that arrives before the header", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    expect(() => assembler.push(bodyFrame(OPERATION, 0, encoder.encode("a"), true))).toThrow(FileStreamError);
  });

  it("refuses a second header", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(0));
    expect(() => assembler.push(textHeader(0))).toThrow(FileStreamError);
  });

  it("accepts an empty file", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(0));
    const body = assembler.finish();
    expect(body.kind).toBe("text");
    expect((body as { bytes: Uint8Array }).bytes).toHaveLength(0);
  });
});

describe("the phone's own cap (§11.1)", () => {
  it("declines the body above 2 MiB, consumes the frames, and reports it too large", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    const total = MAX_CLIENT_FILE_BYTES + 1;
    assembler.push(headerFrame(OPERATION, {
      contentKind: FileContentKind.TEXT,
      totalBytes: BigInt(total),
      contentStreaming: true,
    }));
    // The host keeps streaming; the frames are dropped so the request settles.
    assembler.push(bodyFrame(OPERATION, 0, new Uint8Array(1024)));
    assembler.push(bodyFrame(OPERATION, 1024, new Uint8Array(1024), true));
    expect(assembler.finish()).toEqual({ kind: "tooLarge", size: BigInt(total) });
  });

  it("reads a body of exactly the cap", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(textHeader(MAX_CLIENT_FILE_BYTES));
    assembler.push(bodyFrame(OPERATION, 0, new Uint8Array(MAX_CLIENT_FILE_BYTES), true));
    expect(assembler.finish().kind).toBe("text");
  });
});

describe("classifications the host answers with (§9.7 step 1)", () => {
  it("reports the host's own too-large answer with the metadata size", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    // The host sends `total_bytes: 0` when it streams no body; the size the
    // placeholder shows has to come from the metadata.
    assembler.push(headerFrame(OPERATION, {
      contentKind: FileContentKind.TOO_LARGE,
      totalBytes: 0n,
      contentStreaming: false,
      size: 11n * 1024n * 1024n,
    }));
    expect(assembler.finish()).toEqual({ kind: "tooLarge", size: 11n * 1024n * 1024n });
  });

  it("reports binary and image classifications", () => {
    for (const [kind, expected] of [
      [FileContentKind.BINARY, "binary"],
      [FileContentKind.IMAGE, "image"],
      [FileContentKind.UNSPECIFIED, "unavailable"],
    ] as const) {
      const assembler = new FileStreamAssembler(OPERATION);
      assembler.push(headerFrame(OPERATION, { contentKind: kind, totalBytes: 0n, contentStreaming: false }));
      expect(assembler.finish().kind).toBe(expected);
    }
  });

  it("reports a missing header as unavailable", () => {
    expect(new FileStreamAssembler(OPERATION).finish()).toEqual({ kind: "unavailable" });
  });

  it("refuses a body under a header that declared none", () => {
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(headerFrame(OPERATION, { contentKind: FileContentKind.BINARY, totalBytes: 0n, contentStreaming: false }));
    expect(() => assembler.push(bodyFrame(OPERATION, 0, encoder.encode("x"), true))).toThrow(FileStreamError);
  });

  it("drains the body of an image the host does stream, and still answers `image`", () => {
    // `content_streaming`, not the classification, decides whether frames
    // follow: an image under the host's 25 MiB preview limit streams its bytes
    // (open_stream.rs), and §9.7 shows a placeholder for it either way.
    const assembler = new FileStreamAssembler(OPERATION);
    assembler.push(headerFrame(OPERATION, {
      contentKind: FileContentKind.IMAGE,
      totalBytes: 2048n,
      contentStreaming: true,
    }));
    assembler.push(bodyFrame(OPERATION, 0, new Uint8Array(1024)));
    assembler.push(bodyFrame(OPERATION, 1024, new Uint8Array(1024), true));
    expect(assembler.finish()).toEqual({ kind: "image" });
  });
});

describe("readFile", () => {
  it("streams a file over the bulk lane", async () => {
    const request = vi.fn(async (value: Request, options: { onFileStream: (frame: FileStreamFrame) => void }) => {
      const operationId = value.file!.operationId;
      options.onFileStream(headerFrame(operationId, {
        contentKind: FileContentKind.TEXT,
        totalBytes: 5n,
        contentStreaming: true,
      }));
      options.onFileStream(bodyFrame(operationId, 0, encoder.encode("plain"), true));
      return create(ResponseSchema, { ok: true });
    });
    const body = await readFile(request, { root: "/w", rootToken: "token", path: "/w/a.txt" }, "identity");
    expect(decoder.decode((body as { bytes: Uint8Array }).bytes)).toBe("plain");
  });

  it("surfaces the host's refusal, not a frame error", async () => {
    const request = vi.fn(async () => {
      throw new HostError("bulk_connection_required", "file bodies are allowed only on an independent bulk connection");
    });
    await expect(readFile(request, { root: "/w", rootToken: "token", path: "/w/a.txt" }, "identity")).rejects.toBeInstanceOf(HostError);
  });

  it("holds a frame error until the response has settled the request id", async () => {
    let settled = false;
    const request = vi.fn(async (value: Request, options: { onFileStream: (frame: FileStreamFrame) => void }) => {
      const operationId = value.file!.operationId;
      // A body frame with no header: the assembler throws, and the throw must
      // not escape into the connection's frame loop.
      options.onFileStream(bodyFrame(operationId, 0, encoder.encode("x"), true));
      settled = true;
      return create(ResponseSchema, { ok: true });
    });
    await expect(readFile(request, { root: "/w", rootToken: "token", path: "/w/a.txt" }, "identity")).rejects.toBeInstanceOf(FileStreamError);
    expect(settled).toBe(true);
  });
});

import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  ActiveRootSchema,
  FileContentKind,
  FileMetadataSchema,
  FileServiceResponseSchema,
  FileStreamFrameSchema,
  FileStreamHeaderSchema,
  Operation,
  ResponseSchema,
  type FileStreamFrame,
  type Request,
  type Response,
} from "../../../protocol/gen/envelope_pb";
import { HostError } from "../../../protocol/HostConnection";
import { bodyFrame } from "../../files/testing";
import { AttachedImageError, readAttachedImage, type AttachmentPane, type ImageReadLanes } from "./readImage";

const pane: AttachmentPane = {
  pane: { id: "%7", sessionId: "$2", windowId: "@5", currentPath: "/work" },
  serverIdentity: "server-a",
  topologyGeneration: 40n,
};

const PATH = "/home/dev/.cache/muxflow/uploads/shot.png";

interface Answer {
  contentKind: FileContentKind;
  bytes?: Uint8Array;
  mime?: string;
}

/** A host that resolves `PATH` to a one-file capability and streams `answer` for it. */
function lanes(answer: Answer): { lanes: ImageReadLanes; control: Request[]; bulk: Request[] } {
  const control: Request[] = [];
  const bulk: Request[] = [];
  return {
    control,
    bulk,
    lanes: {
      control: {
        request: async (request) => {
          control.push(request);
          return create(ResponseSchema, {
            ok: true,
            file: create(FileServiceResponseSchema, {
              activeRoot: create(ActiveRootSchema, {
                paneId: "%7",
                root: "/home/dev/.cache/muxflow/uploads",
                rootToken: "file-v2:one-file",
                serverIdentity: "server-a",
                topologyGeneration: 40n,
              }),
              metadata: create(FileMetadataSchema, { path: request.file!.path, name: "shot.png" }),
            }),
          });
        },
      },
      openBulk: async () => ({
        request: async (request: Request, options: { onFileStream: (frame: FileStreamFrame) => void }): Promise<Response> => {
          bulk.push(request);
          const operationId = request.file!.operationId;
          const bytes = answer.bytes;
          options.onFileStream(create(FileStreamFrameSchema, {
            operationId,
            header: create(FileStreamHeaderSchema, {
              contentKind: answer.contentKind,
              totalBytes: BigInt(bytes?.length ?? 0),
              contentStreaming: bytes !== undefined,
              metadata: create(FileMetadataSchema, { path: request.file!.path, name: "shot.png", mime: answer.mime ?? "" }),
            }),
          }));
          if (bytes) options.onFileStream(bodyFrame(operationId, 0, bytes, true));
          return create(ResponseSchema, { ok: true });
        },
      }),
    },
  };
}

describe("reading an attached image back", () => {
  it("resolves the path for the chat's pane, streams it on the bulk lane, and answers its bytes and type", async () => {
    const bytes = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]);
    const host = lanes({ contentKind: FileContentKind.IMAGE, bytes, mime: "image/png" });
    const image = await readAttachedImage(host.lanes, pane, PATH);
    expect(image).toEqual({ bytes, mediaType: "image/png", name: "shot.png" });

    expect(host.control.map((request) => request.operation)).toEqual([Operation.RESOLVE_TERMINAL_FILE]);
    expect(host.control[0]!.file).toMatchObject({
      paneId: "%7",
      path: PATH,
      expectedServerIdentity: "server-a",
      expectedTopologyGeneration: 40n,
      expectedSessionId: "$2",
      expectedWindowId: "@5",
      expectedCwd: "/work",
    });
    expect(host.bulk.map((request) => request.operation)).toEqual([Operation.OPEN_FILE_STREAM]);
    expect(host.bulk[0]!.file).toMatchObject({
      root: "/home/dev/.cache/muxflow/uploads",
      rootToken: "file-v2:one-file",
      path: PATH,
      expectedServerIdentity: "server-a",
    });
  });

  it("names the type from the extension when the host does not", async () => {
    const host = lanes({ contentKind: FileContentKind.IMAGE, bytes: new Uint8Array([1]) });
    expect((await readAttachedImage(host.lanes, pane, PATH)).mediaType).toBe("image/png");
  });

  it("refuses a file that is not an image", async () => {
    for (const contentKind of [FileContentKind.TEXT, FileContentKind.BINARY, FileContentKind.TOO_LARGE]) {
      const host = lanes({ contentKind, ...(contentKind === FileContentKind.TEXT ? { bytes: new Uint8Array([104, 105]) } : {}) });
      const error = await readAttachedImage(host.lanes, pane, PATH).then(() => undefined, (reason: unknown) => reason);
      expect(error).toBeInstanceOf(AttachedImageError);
      expect((error as Error).message).toBe("That attachment is not an image.");
    }
  });

  it("refuses an image over 25 MiB, which the host names but does not stream", async () => {
    const host = lanes({ contentKind: FileContentKind.IMAGE });
    await expect(readAttachedImage(host.lanes, pane, PATH)).rejects.toThrow("That image is too large to show.");
  });

  it("asks the host nothing for an attachment with no path", async () => {
    const host = lanes({ contentKind: FileContentKind.IMAGE, bytes: new Uint8Array([1]) });
    await expect(readAttachedImage(host.lanes, pane, "")).rejects.toThrow("That image is not a file on the host.");
    expect(host.control).toEqual([]);
    expect(host.bulk).toEqual([]);
  });

  it("passes on the host's own refusal", async () => {
    const host = lanes({ contentKind: FileContentKind.IMAGE });
    host.lanes.control.request = async () => {
      throw new HostError("terminal_file_rejected", "No such file or directory");
    };
    const error = await readAttachedImage(host.lanes, pane, PATH).then(() => undefined, (reason: unknown) => reason);
    expect(error).toBeInstanceOf(HostError);
    expect((error as Error).message).toBe("No such file or directory");
    expect(host.bulk).toEqual([]);
  });
});

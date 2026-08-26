// Message builders for the Files tests. Kept beside the tests so no test
// hand-writes a proto shape (design doc D5).

import { create } from "@bufbuild/protobuf";
import {
  DirectorySnapshotSchema,
  FileContentKind,
  FileKind,
  FileMetadataSchema,
  FileServiceResponseSchema,
  FileStreamFrameSchema,
  FileStreamHeaderSchema,
  ResponseSchema,
  type FileMetadata,
  type FileStreamFrame,
  type Response,
} from "../../protocol/gen/envelope_pb";

export function metadata(name: string, options: Partial<Omit<FileMetadata, "$typeName">> = {}): FileMetadata {
  return create(FileMetadataSchema, {
    path: `/w/${name}`,
    name,
    kind: FileKind.FILE,
    size: 0n,
    ...options,
  });
}

export function directory(name: string, options: Partial<Omit<FileMetadata, "$typeName">> = {}): FileMetadata {
  return metadata(name, { kind: FileKind.DIRECTORY, ...options });
}

export function listingResponse(
  entries: FileMetadata[],
  options: { nextPageToken?: string; path?: string } = {},
): Response {
  const nextPageToken = options.nextPageToken ?? "";
  return create(ResponseSchema, {
    ok: true,
    file: create(FileServiceResponseSchema, {
      directory: create(DirectorySnapshotSchema, {
        root: "/w",
        path: options.path ?? "/w",
        entries,
        nextPageToken,
        complete: nextPageToken === "",
        overflowed: nextPageToken !== "",
        authoritative: true,
      }),
    }),
  });
}

export function headerFrame(
  operationId: string,
  options: { contentKind: FileContentKind; totalBytes: bigint; contentStreaming: boolean; size?: bigint },
): FileStreamFrame {
  return create(FileStreamFrameSchema, {
    operationId,
    header: create(FileStreamHeaderSchema, {
      contentKind: options.contentKind,
      totalBytes: options.totalBytes,
      contentStreaming: options.contentStreaming,
      metadata: metadata("file.txt", { size: options.size ?? options.totalBytes }),
    }),
  });
}

export function bodyFrame(operationId: string, offset: number, data: Uint8Array, eof = false): FileStreamFrame {
  return create(FileStreamFrameSchema, { operationId, offset: BigInt(offset), data, eof });
}

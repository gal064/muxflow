// The chat's files bound to the app: which connection each part goes over,
// and where an image read back from the host is kept on the phone. The
// connections, the phone's file system and the two transfers are stand-ins.

import { beforeEach, describe, expect, it, vi } from "vitest";

const { disk, lanes, transfers } = vi.hoisted(() => ({
  /** The phone's files by URI. */
  disk: new Map<string, Uint8Array>(),
  lanes: {
    control: undefined as { state: string } | undefined,
    openBulkConnection: () => Promise.resolve({ lane: "shared" }),
    openUploadConnection: () => Promise.resolve({ lane: "upload" }),
    toasts: [] as string[],
  },
  transfers: {
    uploads: [] as { openLane: unknown; source: { name: string; size: number } }[],
    reads: [] as { control: unknown; openBulk: unknown; pane: unknown; hostPath: string }[],
    image: { bytes: new Uint8Array([1, 2, 3]), mediaType: "image/png", name: "desktop.png" },
  },
}));

vi.mock("expo-file-system", () => {
  const uriOf = (parts: (string | { uri: string })[]): string => parts.map((part) => (typeof part === "string" ? part : part.uri)).join("/");
  class Directory {
    readonly uri: string;
    constructor(...parts: (string | { uri: string })[]) {
      this.uri = uriOf(parts);
    }
    get exists(): boolean {
      return disk.has(`${this.uri}/`);
    }
    create(): void {
      disk.set(`${this.uri}/`, new Uint8Array(0));
    }
  }
  class File {
    readonly uri: string;
    constructor(...parts: (string | { uri: string })[]) {
      this.uri = uriOf(parts);
    }
    get exists(): boolean {
      return disk.has(this.uri);
    }
    get name(): string {
      return this.uri.slice(this.uri.lastIndexOf("/") + 1);
    }
    get size(): number {
      return disk.get(this.uri)?.length ?? 0;
    }
    write(bytes: Uint8Array): void {
      disk.set(this.uri, bytes);
    }
    delete(): void {
      disk.delete(this.uri);
    }
  }
  return { Directory, File, FileMode: { ReadOnly: "r" }, Paths: { cache: { uri: "file:///cache" } } };
});
vi.mock("../../../session/connectionManager", () => ({
  getConnection: () => lanes.control,
  openBulkConnection: lanes.openBulkConnection,
  openUploadConnection: lanes.openUploadConnection,
  toast: (message: string) => lanes.toasts.push(message),
}));
vi.mock("./upload", () => ({
  uploadFile: async (options: { openLane: unknown; source: { name: string; size: number } }) => {
    transfers.uploads.push({ openLane: options.openLane, source: options.source });
    return { hostPath: `/uploads/${options.source.name}`, name: options.source.name, sizeBytes: options.source.size };
  },
}));
vi.mock("./readImage", () => ({
  readAttachedImage: async (given: { control: unknown; openBulk: unknown }, pane: unknown, hostPath: string) => {
    transfers.reads.push({ control: given.control, openBulk: given.openBulk, pane, hostPath });
    return transfers.image;
  },
}));

import { sessionStore, type Pane } from "../../../store/sessionStore";
import { attachDeps, chatImages } from "./attachRuntime";

const PANE: Pane = { id: "%7", sessionId: "$2", windowId: "@5", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/work", currentCommand: "claude", chatAdapterId: "claude" };

beforeEach(() => {
  disk.clear();
  transfers.uploads.length = 0;
  transfers.reads.length = 0;
  transfers.image = { bytes: new Uint8Array([1, 2, 3]), mediaType: "image/png", name: "desktop.png" };
  lanes.control = { state: "connected" };
  lanes.toasts.length = 0;
  sessionStore.setState({ panes: { "%7": PANE }, serverIdentity: "server-a", topologyGeneration: 40n });
});

describe("uploading", () => {
  it("runs each upload on a bulk lane of its own, not the one the file viewer reads on", async () => {
    const source = { name: "shot.png", size: 3, read: async () => new Uint8Array(3) };
    await attachDeps.upload(source, new AbortController().signal, () => undefined);
    expect(transfers.uploads).toHaveLength(1);
    expect(transfers.uploads[0]!.openLane).toBe(lanes.openUploadConnection);
    expect(transfers.uploads[0]!.openLane).not.toBe(lanes.openBulkConnection);
  });

  it("reads a chosen file as the copy the picker made, under the name the picker gave", () => {
    disk.set("file:///cache/ImagePicker/0c2d.jpg", new Uint8Array(900));
    const source = attachDeps.toSource({ uri: "file:///cache/ImagePicker/0c2d.jpg", name: "IMG_0001.JPG", size: 4096 });
    // The copy's own size is what will be read, whatever the picker said of the original.
    expect(source).toMatchObject({ name: "IMG_0001.JPG", size: 900, localUri: "file:///cache/ImagePicker/0c2d.jpg" });
    expect(attachDeps.toSource({ uri: "file:///cache/ImagePicker/0c2d.jpg" }).name).toBe("0c2d.jpg");
  });

  it("refuses a chosen file that is no longer on the phone", () => {
    expect(() => attachDeps.toSource({ uri: "file:///cache/gone.txt", name: "gone.txt" })).toThrow("That file is no longer on the phone.");
  });

  it("says a refusal with no chip as a toast", () => {
    attachDeps.say("A message takes at most 10 attachments.");
    expect(lanes.toasts).toEqual(["A message takes at most 10 attachments."]);
  });
});

describe("reading an image back", () => {
  it("resolves on the control connection, streams on the shared bulk lane, and keeps the image in the cache", async () => {
    const { uri } = await chatImages("%7").load("/uploads/desktop.png");
    expect(transfers.reads).toEqual([{
      control: lanes.control,
      openBulk: lanes.openBulkConnection,
      pane: { pane: PANE, serverIdentity: "server-a", topologyGeneration: 40n },
      hostPath: "/uploads/desktop.png",
    }]);
    expect(uri).toMatch(/^file:\/\/\/cache\/chat-images\/[a-z0-9]+-\d+\.png$/);
    expect(disk.get(uri)).toEqual(new Uint8Array([1, 2, 3]));
  });

  it("gives each image a file of its own, by its kind, and removes it when the viewer closes", async () => {
    const images = chatImages("%7");
    const first = await images.load("/uploads/desktop.png");
    transfers.image = { bytes: new Uint8Array([9]), mediaType: "image/jpeg", name: "photo.jpg" };
    const second = await images.load("/uploads/photo.jpg");
    expect(second.uri).not.toBe(first.uri);
    expect(second.uri.endsWith(".jpg")).toBe(true);
    images.release(first.uri);
    expect(disk.has(first.uri)).toBe(false);
    expect(disk.has(second.uri)).toBe(true);
    // A file the system already cleared is not an error.
    images.release(first.uri);
  });

  it("says so when the phone is not connected, or the chat's pane has closed", async () => {
    lanes.control = { state: "reconnecting" };
    await expect(chatImages("%7").load("/uploads/desktop.png")).rejects.toThrow("Not connected.");
    lanes.control = undefined;
    await expect(chatImages("%7").load("/uploads/desktop.png")).rejects.toThrow("Not connected.");
    lanes.control = { state: "connected" };
    await expect(chatImages("%9").load("/uploads/desktop.png")).rejects.toThrow("This chat's pane has closed.");
    expect(transfers.reads).toEqual([]);
  });
});

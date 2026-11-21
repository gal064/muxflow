// One chat's files, from the attach sheet's three choices to what a send
// takes. The pickers are the real ones over stubbed native modules
// (src/testing/stubs); the upload is settled by hand.

import { act, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it } from "vitest";

import { render } from "../../../testing/render";
import { documentPicker } from "../../../testing/stubs/expoDocumentPicker";
import { imagePicker } from "../../../testing/stubs/expoImagePicker";
import { MAX_ATTACHMENTS, type Uploader } from "./attachmentQueue";
import { clearLocalImages, localImageFor } from "./localImages";
import { pickFiles, type PickedFile } from "./pickers";
import { UploadError, type UploadedFile } from "./upload";
import { useAttachments, type AttachDeps, type ChatAttachments } from "./useAttachments";

interface Started {
  name: string;
  signal: AbortSignal;
  progress: (sentBytes: number, totalBytes: number) => void;
  finish: () => void;
  fail: (error: unknown) => void;
}

function setup(paneId = "%7") {
  const started: Started[] = [];
  const said: string[] = [];
  const upload: Uploader = (file, signal, progress) =>
    new Promise<UploadedFile>((resolve, reject) => {
      signal.addEventListener("abort", () => reject(new UploadError("The upload was cancelled.", true)));
      started.push({
        name: file.name,
        signal,
        progress,
        finish: () => resolve({ hostPath: `/uploads/${file.name}`, name: file.name, sizeBytes: file.size }),
        fail: reject,
      });
    });
  const deps: AttachDeps = {
    upload,
    pick: pickFiles,
    toSource: (file: PickedFile) => {
      if (file.uri.includes("gone")) throw new Error("That file is no longer on the phone.");
      return { name: file.name ?? file.uri.slice(file.uri.lastIndexOf("/") + 1), size: file.size ?? 1000, localUri: file.uri, read: async () => new Uint8Array(0) };
    },
    say: (message) => said.push(message),
  };
  const held: { current: ChatAttachments | undefined } = { current: undefined };
  function Screen({ pane }: { pane: string }) {
    held.current = useAttachments(pane, deps);
    return null;
  }
  const view: ReactTestRenderer = render(<Screen pane={paneId} />);
  return {
    view,
    started,
    said,
    get files(): ChatAttachments {
      return held.current!;
    },
  };
}

/** Lets the picker's answer and the queue's promise chain run. */
async function settle(): Promise<void> {
  await act(async () => {
    for (let turn = 0; turn < 10; turn += 1) await Promise.resolve();
  });
}

const chips = (files: ChatAttachments): string[] => files.entries.map((entry) => `${entry.name}:${entry.status}`);

beforeEach(() => {
  imagePicker.reset();
  documentPicker.reset();
  clearLocalImages();
});

describe("the attach sheet's choices", () => {
  it("Photo library: every photo chosen becomes a chip and uploads in its turn", async () => {
    const chat = setup();
    imagePicker.library = [
      { uri: "file:///cache/a.jpg", fileName: "IMG_0001.JPG", fileSize: 4096 },
      { uri: "file:///cache/b.png", fileName: "IMG_0002.PNG", fileSize: 2048 },
    ];
    act(() => chat.files.choose("library"));
    await settle();
    expect(chips(chat.files)).toEqual(["IMG_0001.JPG:uploading", "IMG_0002.PNG:queued"]);
    expect(chat.files.entries[0]).toMatchObject({ localUri: "file:///cache/a.jpg", mediaType: "image/jpeg", sizeBytes: 4096 });
    // Asked for as many as still fit.
    expect(imagePicker.launches[0]!.options.selectionLimit).toBe(MAX_ATTACHMENTS);

    act(() => chat.started[0]!.progress(1024, 4096));
    expect(chat.files.entries[0]).toMatchObject({ sentBytes: 1024, status: "uploading" });
    chat.started[0]!.finish();
    await settle();
    expect(chips(chat.files)).toEqual(["IMG_0001.JPG:ready", "IMG_0002.PNG:uploading"]);
  });

  it("Take a photo: the photo becomes a chip", async () => {
    const chat = setup();
    imagePicker.camera = [{ uri: "file:///cache/0c2d.jpg", fileSize: 900 }];
    act(() => chat.files.choose("camera"));
    await settle();
    expect(chat.files.entries).toHaveLength(1);
    expect(chat.files.entries[0]!.name).toMatch(/^Photo \d{4}-\d{2}-\d{2} \d{2}\.\d{2}\.\d{2}\.jpg$/);
    expect(chat.files.entries[0]).toMatchObject({ status: "uploading", mediaType: "image/jpeg", localUri: "file:///cache/0c2d.jpg" });
  });

  it("File: each file chosen becomes a chip", async () => {
    const chat = setup();
    documentPicker.files = [{ uri: "file:///cache/layout.log", name: "layout.log", size: 18_432 }];
    act(() => chat.files.choose("file"));
    await settle();
    expect(chips(chat.files)).toEqual(["layout.log:uploading"]);
    expect(chat.files.entries[0]).toMatchObject({ mediaType: "", sizeBytes: 18_432 });
  });

  it("backing out of a picker adds nothing and says nothing", async () => {
    const chat = setup();
    for (const choice of ["library", "camera", "file"] as const) act(() => chat.files.choose(choice));
    await settle();
    expect(chat.files.entries).toEqual([]);
    expect(chat.said).toEqual([]);
  });

  it("says why when the camera may not be used, or a chosen file cannot be read", async () => {
    const chat = setup();
    imagePicker.cameraGranted = false;
    act(() => chat.files.choose("camera"));
    await settle();
    documentPicker.files = [{ uri: "file:///cache/gone.txt", name: "gone.txt" }, { uri: "file:///cache/here.txt", name: "here.txt" }];
    act(() => chat.files.choose("file"));
    await settle();
    expect(chat.said).toEqual([
      "Muxflow may not use the camera. Allow it in the phone's settings to take a photo.",
      "That file is no longer on the phone.",
    ]);
    // The file that could be read is attached all the same.
    expect(chips(chat.files)).toEqual(["here.txt:uploading"]);
  });
});

describe("the most a message carries", () => {
  it("refuses the eleventh file in the desktop's words, and keeps the ten", async () => {
    const chat = setup();
    documentPicker.files = Array.from({ length: 11 }, (_, index) => ({ uri: `file:///cache/${index}.txt`, name: `${index}.txt` }));
    act(() => chat.files.choose("file"));
    await settle();
    expect(chat.files.entries).toHaveLength(10);
    expect(chat.said).toEqual(["A message takes at most 10 attachments."]);
  });

  it("asks the library only for what still fits, and opens nothing when the field is full", async () => {
    const chat = setup();
    documentPicker.files = Array.from({ length: 8 }, (_, index) => ({ uri: `file:///cache/${index}.txt`, name: `${index}.txt` }));
    act(() => chat.files.choose("file"));
    await settle();
    act(() => chat.files.choose("library"));
    await settle();
    expect(imagePicker.launches[0]!.options.selectionLimit).toBe(2);

    documentPicker.files = [{ uri: "file:///cache/8.txt", name: "8.txt" }, { uri: "file:///cache/9.txt", name: "9.txt" }];
    act(() => chat.files.choose("file"));
    await settle();
    expect(chat.files.entries).toHaveLength(10);
    const opened = documentPicker.opened.length;
    act(() => chat.files.choose("file"));
    act(() => chat.files.choose("camera"));
    await settle();
    expect(documentPicker.opened).toHaveLength(opened);
    expect(imagePicker.launches.filter((launch) => launch.from === "camera")).toEqual([]);
    expect(chat.said).toEqual(["A message takes at most 10 attachments.", "A message takes at most 10 attachments."]);
  });

  it("puts an image over the desktop's limit on the field as failed, with the desktop's words", async () => {
    const chat = setup();
    imagePicker.library = [{ uri: "file:///cache/huge.png", fileName: "huge.png", fileSize: 25 * 1024 * 1024 + 1 }];
    act(() => chat.files.choose("library"));
    await settle();
    expect(chat.files.entries[0]).toMatchObject({ status: "failed", error: "huge.png is over 25 MB, the most an image can be." });
    expect(chat.started).toEqual([]);
  });
});

describe("removing and leaving", () => {
  it("removing a chip stops its upload, and the next file goes", async () => {
    const chat = setup();
    documentPicker.files = [{ uri: "file:///cache/a.txt", name: "a.txt" }, { uri: "file:///cache/b.txt", name: "b.txt" }];
    act(() => chat.files.choose("file"));
    await settle();
    act(() => chat.files.remove(chat.files.entries[0]!.id));
    expect(chat.started[0]!.signal.aborted).toBe(true);
    await settle();
    expect(chips(chat.files)).toEqual(["b.txt:uploading"]);
  });

  it("leaving the chat stops the upload in flight", async () => {
    const chat = setup();
    documentPicker.files = [{ uri: "file:///cache/a.txt", name: "a.txt" }, { uri: "file:///cache/b.txt", name: "b.txt" }];
    act(() => chat.files.choose("file"));
    await settle();
    expect(chat.started).toHaveLength(1);
    act(() => chat.view.unmount());
    await settle();
    expect(chat.started[0]!.signal.aborted).toBe(true);
    // The file that waited never starts.
    expect(chat.started).toHaveLength(1);
  });

  it("uploads nothing for a chat that was left while the picker was open", async () => {
    const chat = setup();
    documentPicker.files = [{ uri: "file:///cache/a.txt", name: "a.txt" }];
    act(() => chat.files.choose("file"));
    act(() => chat.view.unmount());
    await settle();
    expect(chat.started).toEqual([]);
  });
});

describe("what a send takes", () => {
  async function withTwoReady() {
    const chat = setup();
    imagePicker.library = [{ uri: "file:///cache/shot.png", fileName: "shot.png", fileSize: 2048 }];
    act(() => chat.files.choose("library"));
    documentPicker.files = [{ uri: "file:///cache/layout.log", name: "layout.log", size: 18_432 }];
    act(() => chat.files.choose("file"));
    await settle();
    chat.started[0]!.finish();
    await settle();
    chat.started[1]!.finish();
    await settle();
    expect(chips(chat.files)).toEqual(["shot.png:ready", "layout.log:ready"]);
    return chat;
  }

  it("the files as the message carries them, off the reply field while it is on its way", async () => {
    const chat = await withTwoReady();
    let taken!: ReturnType<ChatAttachments["take"]>;
    act(() => {
      taken = chat.files.take();
    });
    expect(taken.files).toEqual([
      { path: "/uploads/shot.png", name: "shot.png", mediaType: "image/png", sizeBytes: 2048, localUri: "file:///cache/shot.png" },
      { path: "/uploads/layout.log", name: "layout.log", mediaType: "", sizeBytes: 18_432, localUri: "file:///cache/layout.log" },
    ]);
    expect(chat.files.entries).toEqual([]);
    // Known before the host shows the message back, so its chip is a thumbnail from the first draw.
    expect(localImageFor("/uploads/shot.png", 2048)).toBe("file:///cache/shot.png");
    expect(localImageFor("/uploads/layout.log", 18_432)).toBeUndefined();
  });

  it("a send the host took empties the field for good; a file added meanwhile stays", async () => {
    const chat = await withTwoReady();
    let taken!: ReturnType<ChatAttachments["take"]>;
    act(() => {
      taken = chat.files.take();
    });
    documentPicker.files = [{ uri: "file:///cache/late.txt", name: "late.txt" }];
    act(() => chat.files.choose("file"));
    await settle();
    act(() => taken.sent());
    expect(chips(chat.files)).toEqual(["late.txt:uploading"]);
  });

  it("a refused send puts the files back on the field", async () => {
    const chat = await withTwoReady();
    let taken!: ReturnType<ChatAttachments["take"]>;
    act(() => {
      taken = chat.files.take();
    });
    act(() => taken.back());
    expect(chips(chat.files)).toEqual(["shot.png:ready", "layout.log:ready"]);
  });

  it("an edited message's files replace the field's, with nothing uploaded", async () => {
    const chat = await withTwoReady();
    act(() => chat.files.replace([
      { path: "/uploads/shot.png", name: "shot.png", mediaType: "image/png", sizeBytes: 2048 },
      { path: "/uploads/spec.pdf", name: "spec.pdf", mediaType: "", sizeBytes: 90_000 },
    ]));
    expect(chips(chat.files)).toEqual(["shot.png:ready", "spec.pdf:ready"]);
    expect(chat.started).toHaveLength(2);
    let taken!: ReturnType<ChatAttachments["take"]>;
    act(() => {
      taken = chat.files.take();
    });
    expect(taken.files.map((file) => file.path)).toEqual(["/uploads/shot.png", "/uploads/spec.pdf"]);
    act(() => taken.back());
    act(() => chat.files.clear());
    expect(chat.files.entries).toEqual([]);
  });
});

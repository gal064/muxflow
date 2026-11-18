import { describe, expect, it } from "vitest";
import { AttachmentQueue, MAX_ATTACHMENTS, type AttachmentEntry, type AttachmentSource, type Uploader } from "./attachmentQueue";
import { MAX_IMAGE_UPLOAD_BYTES, MAX_UPLOAD_BYTES, UploadError, type UploadedFile } from "./upload";

function source(name: string, size = 1000, localUri?: string): AttachmentSource {
  return { name, size, ...(localUri === undefined ? {} : { localUri }), read: async () => new Uint8Array(0) };
}

interface Started {
  name: string;
  signal: AbortSignal;
  progress: (sentBytes: number, totalBytes: number) => void;
  finish: (uploaded?: Partial<UploadedFile>) => void;
  fail: (error: unknown) => void;
}

/** An uploader the test settles by hand. An abort rejects it, as the real one does. */
function fakeUploader(): { upload: Uploader; started: Started[] } {
  const started: Started[] = [];
  const upload: Uploader = (file, signal, progress) =>
    new Promise<UploadedFile>((resolve, reject) => {
      signal.addEventListener("abort", () => reject(new UploadError("The upload was cancelled.", true)));
      started.push({
        name: file.name,
        signal,
        progress,
        finish: (uploaded = {}) => resolve({ hostPath: `/uploads/${file.name}`, name: file.name, sizeBytes: file.size, ...uploaded }),
        fail: reject,
      });
    });
  return { upload, started };
}

/** Lets the queue's promise chain run. */
async function settle(): Promise<void> {
  for (let turn = 0; turn < 8; turn += 1) await Promise.resolve();
}

function added(queue: AttachmentQueue, file: AttachmentSource): AttachmentEntry {
  const result = queue.add(file);
  if (!("entry" in result)) throw new Error(result.refused);
  return result.entry;
}

const statuses = (queue: AttachmentQueue): string[] => queue.entries.map((entry) => `${entry.name}:${entry.status}`);

describe("the attachment queue", () => {
  it("describes a file as its chip draws it", async () => {
    const { upload } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const entry = added(queue, source("IMG_0001.JPG", 4096, "file:///cache/IMG_0001.JPG"));
    expect(entry).toEqual({
      id: entry.id,
      name: "IMG_0001.JPG",
      sizeBytes: 4096,
      mediaType: "image/jpeg",
      localUri: "file:///cache/IMG_0001.JPG",
      status: "queued",
      sentBytes: 0,
    });
    expect(added(queue, source("notes.txt")).mediaType).toBe("");
    // Plain data: the store can be read, compared and written out as it is.
    expect(JSON.parse(JSON.stringify(queue.store.getState()))).toEqual(queue.store.getState());
  });

  it("uploads one file at a time, in the order they were added", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    added(queue, source("a.png"));
    added(queue, source("b.txt"));
    await settle();
    expect(statuses(queue)).toEqual(["a.png:uploading", "b.txt:queued"]);
    expect(started.map((upload) => upload.name)).toEqual(["a.png"]);
    expect(queue.ready()).toBe(false);

    started[0]!.progress(400, 1000);
    expect(queue.entries[0]).toMatchObject({ sentBytes: 400, status: "uploading" });

    started[0]!.finish({ hostPath: "/uploads/a (1).png", name: "a (1).png" });
    await settle();
    expect(statuses(queue)).toEqual(["a.png:ready", "b.txt:uploading"]);
    // The entry keeps the file's own name; the path carries the host's.
    expect(queue.entries[0]).toMatchObject({ name: "a.png", hostPath: "/uploads/a (1).png", sentBytes: 1000 });
    expect(queue.ready()).toBe(false);

    started[1]!.finish();
    await settle();
    expect(queue.ready()).toBe(true);
    expect(queue.attachmentsForSend()).toEqual([
      { path: "/uploads/a (1).png", name: "a.png" },
      { path: "/uploads/b.txt", name: "b.txt" },
    ]);
  });

  it("tells subscribers of every change", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const seen: string[] = [];
    const unsubscribe = queue.store.subscribe((state) => seen.push(state.entries.map((entry) => `${entry.status}@${entry.sentBytes}`).join(",")));
    added(queue, source("a.png"));
    await settle();
    started[0]!.progress(500, 1000);
    started[0]!.finish();
    await settle();
    unsubscribe();
    expect(seen).toEqual(["queued@0", "uploading@0", "uploading@500", "ready@1000"]);
  });

  it("says on the chip why an upload failed, and goes on to the next", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    added(queue, source("a.png"));
    added(queue, source("b.txt"));
    await settle();
    started[0]!.fail(new UploadError("insufficient destination space: need 1000 bytes, have 0"));
    await settle();
    expect(queue.entries[0]).toMatchObject({ status: "failed", error: "insufficient destination space: need 1000 bytes, have 0" });
    expect(statuses(queue)).toEqual(["a.png:failed", "b.txt:uploading"]);
    started[1]!.finish();
    await settle();
    // A failed file keeps Send waiting until it is taken out.
    expect(queue.ready()).toBe(false);
    expect(queue.attachmentsForSend()).toEqual([{ path: "/uploads/b.txt", name: "b.txt" }]);
    queue.remove(queue.entries[0]!.id);
    expect(queue.ready()).toBe(true);
  });

  it("fails a file whose uploader throws before it starts", async () => {
    const queue = new AttachmentQueue(() => {
      throw new Error("not connected");
    });
    added(queue, source("a.png"));
    await settle();
    expect(queue.entries[0]).toMatchObject({ status: "failed", error: "not connected" });
  });

  it("adds a file the phone will not send as failed, without uploading it", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const image = added(queue, source("huge.png", MAX_IMAGE_UPLOAD_BYTES + 1));
    const file = added(queue, source("huge.iso", MAX_UPLOAD_BYTES + 1));
    await settle();
    expect(image).toMatchObject({ status: "failed", error: "huge.png is over 25 MB, the most an image can be." });
    expect(file).toMatchObject({ status: "failed", error: "huge.iso is over 100 MB, the most the phone can send." });
    expect(started).toEqual([]);
    expect(queue.ready()).toBe(false);
  });

  it("stops the upload of a file that is removed, and starts the next once that ended", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const first = added(queue, source("a.png"));
    added(queue, source("b.txt"));
    await settle();
    queue.remove(first.id);
    expect(started[0]!.signal.aborted).toBe(true);
    expect(statuses(queue)).toEqual(["b.txt:queued"]);
    await settle();
    expect(statuses(queue)).toEqual(["b.txt:uploading"]);
    // A late word from the removed upload changes nothing.
    started[0]!.progress(900, 1000);
    started[0]!.finish();
    await settle();
    expect(statuses(queue)).toEqual(["b.txt:uploading"]);
  });

  it("drops a waiting or a finished file without touching the one that runs", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const first = added(queue, source("a.png"));
    added(queue, source("b.txt"));
    const third = added(queue, source("c.txt"));
    await settle();
    started[0]!.finish();
    await settle();
    queue.remove(first.id);
    queue.remove(third.id);
    queue.remove("no-such-id");
    expect(statuses(queue)).toEqual(["b.txt:uploading"]);
    expect(started[1]!.signal.aborted).toBe(false);
    started[1]!.finish();
    await settle();
    expect(started.map((upload) => upload.name)).toEqual(["a.png", "b.txt"]);
    expect(queue.ready()).toBe(true);
  });

  it("takes at most 10 files, and says so in the host's words", async () => {
    const { upload } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    for (let index = 0; index < MAX_ATTACHMENTS; index += 1) added(queue, source(`f${index}.txt`));
    expect(queue.add(source("one-more.txt"))).toEqual({ refused: "A message takes at most 10 attachments." });
    expect(queue.entries).toHaveLength(10);
    // Room again once one is taken out.
    queue.remove(queue.entries[9]!.id);
    expect("entry" in queue.add(source("one-more.txt"))).toBe(true);
  });

  it("is not ready with nothing attached", () => {
    const queue = new AttachmentQueue(fakeUploader().upload);
    expect(queue.ready()).toBe(false);
    expect(queue.attachmentsForSend()).toEqual([]);
  });

  it("clears after a send, stopping what still runs, and takes new files after", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const before = added(queue, source("a.png"));
    added(queue, source("b.txt"));
    await settle();
    started[0]!.finish();
    await settle();
    queue.clear();
    expect(queue.entries).toEqual([]);
    expect(started[1]!.signal.aborted).toBe(true);
    await settle();

    const after = added(queue, source("c.txt"));
    expect(after.id).not.toBe(before.id);
    await settle();
    expect(statuses(queue)).toEqual(["c.txt:uploading"]);
    expect(started).toHaveLength(3);
  });

  it("takes a file the host already holds as ready, with nothing uploaded", async () => {
    const { upload, started } = fakeUploader();
    const queue = new AttachmentQueue(upload);
    const hosted = queue.addHosted({ path: "/uploads/shot.png", name: "shot.png", mediaType: "", sizeBytes: 2048, localUri: "file:///cache/shot.png" });
    if (!("entry" in hosted)) throw new Error(hosted.refused);
    expect(hosted.entry).toEqual({
      id: hosted.entry.id,
      name: "shot.png",
      sizeBytes: 2048,
      // Not said by the host: read from the name, as for a file from the phone.
      mediaType: "image/png",
      localUri: "file:///cache/shot.png",
      status: "ready",
      sentBytes: 2048,
      hostPath: "/uploads/shot.png",
    });
    await settle();
    expect(started).toEqual([]);
    expect(queue.ready()).toBe(true);
    expect(queue.attachmentsForSend()).toEqual([{ path: "/uploads/shot.png", name: "shot.png" }]);

    // It counts toward the ten like any other.
    for (let index = 1; index < MAX_ATTACHMENTS; index += 1) queue.addHosted({ path: `/uploads/${index}.txt`, name: `${index}.txt`, mediaType: "", sizeBytes: 1 });
    expect(queue.addHosted({ path: "/uploads/more.txt", name: "more.txt", mediaType: "", sizeBytes: 1 })).toEqual({ refused: "A message takes at most 10 attachments." });
  });
});

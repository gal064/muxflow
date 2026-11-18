import { describe, expect, it } from "vitest";

import type { AttachmentEntry } from "./attachmentQueue";
import { chipLine, FILES_FAILED, FILES_UPLOADING, filesRefusal, filesState, isImage, sizeLabel, uploadShare } from "./attachState";
import { clearLocalImages, forgetLocalImage, localImageFor, rememberLocalImage } from "./localImages";

function entry(fields: Partial<AttachmentEntry> = {}): AttachmentEntry {
  return { id: "attachment-1", name: "layout.log", sizeBytes: 18_432, mediaType: "", status: "ready", sentBytes: 18_432, ...fields };
}

describe("what the files say about sending", () => {
  it("holds Send while one uploads or waits, and after one failed", () => {
    expect(filesState([])).toBe("none");
    expect(filesState([entry()])).toBe("ready");
    expect(filesState([entry(), entry({ status: "uploading" })])).toBe("uploading");
    expect(filesState([entry(), entry({ status: "queued" })])).toBe("uploading");
    // A failed file is said first: it does not finish by waiting.
    expect(filesState([entry({ status: "uploading" }), entry({ status: "failed" })])).toBe("failed");
  });

  it("says why in the desktop's words", () => {
    expect(filesRefusal("uploading")).toBe("The attachments are still uploading.");
    expect(FILES_UPLOADING).toBe("The attachments are still uploading.");
    expect(filesRefusal("failed")).toBe(FILES_FAILED);
    expect(filesRefusal("ready")).toBeUndefined();
    expect(filesRefusal("none")).toBeUndefined();
  });
});

describe("a chip's words", () => {
  it("gives a size as the desktop does, in binary units", () => {
    expect(sizeLabel(0)).toBe("0 B");
    expect(sizeLabel(1023)).toBe("1023 B");
    expect(sizeLabel(18_432)).toBe("18 KB");
    expect(sizeLabel(880_640)).toBe("860 KB");
    expect(sizeLabel(1_468_006)).toBe("1.4 MB");
    expect(sizeLabel(25n * 1024n * 1024n)).toBe("25.0 MB");
  });

  it("says how far an upload is, and why one failed", () => {
    expect(chipLine(entry())).toBe("18 KB");
    expect(chipLine(entry({ status: "queued", sentBytes: 0 }))).toBe("Waiting to upload");
    expect(chipLine(entry({ status: "uploading", sentBytes: 7_373 }))).toBe("Uploading 40%");
    expect(chipLine(entry({ status: "failed", error: "The connection to the host was lost." }))).toBe("The connection to the host was lost.");
    expect(chipLine(entry({ status: "failed" }))).toBe("The upload failed.");
  });

  it("keeps the share of an upload between nothing and all of it", () => {
    expect(uploadShare(entry({ status: "uploading", sentBytes: 0 }))).toBe(0);
    expect(uploadShare(entry({ status: "uploading", sentBytes: 9_216 }))).toBe(0.5);
    expect(uploadShare(entry({ status: "uploading", sentBytes: 99_999 }))).toBe(1);
    // An empty file has nothing to send, and is whole once it is ready.
    expect(uploadShare(entry({ status: "uploading", sizeBytes: 0, sentBytes: 0 }))).toBe(0);
    expect(uploadShare(entry({ status: "ready", sizeBytes: 0, sentBytes: 0 }))).toBe(1);
  });

  it("takes an image by its type, and by its name when the type is not known", () => {
    expect(isImage({ mediaType: "image/png", name: "shot" })).toBe(true);
    expect(isImage({ mediaType: "", name: "IMG_0001.JPG" })).toBe(true);
    expect(isImage({ mediaType: "", name: "layout.log" })).toBe(false);
    expect(isImage({ mediaType: "application/pdf", name: "scan.png" })).toBe(false);
  });
});

describe("where this phone's images are", () => {
  it("remembers an image by its path on the host, until it is forgotten", () => {
    clearLocalImages();
    rememberLocalImage("/uploads/shot.png", "file:///cache/shot.png", 2048);
    expect(localImageFor("/uploads/shot.png", 2048)).toBe("file:///cache/shot.png");
    expect(localImageFor("/uploads/other.png", 2048)).toBeUndefined();
    forgetLocalImage("/uploads/shot.png");
    expect(localImageFor("/uploads/shot.png", 2048)).toBeUndefined();
  });

  it("is not taken for another file the host later keeps under the same name", () => {
    clearLocalImages();
    rememberLocalImage("/uploads/shot.png", "file:///cache/shot.png", 2048);
    expect(localImageFor("/uploads/shot.png", 4096)).toBeUndefined();
    // A host that gives no size is taken at its word about the path.
    expect(localImageFor("/uploads/shot.png", 0)).toBe("file:///cache/shot.png");
  });

  it("has nothing for an image with no path, and keeps only the newest", () => {
    clearLocalImages();
    rememberLocalImage("", "file:///cache/inline.png", 10);
    expect(localImageFor("", 10)).toBeUndefined();
    for (let index = 0; index < 201; index += 1) rememberLocalImage(`/uploads/${index}.png`, `file:///cache/${index}.png`, 10);
    expect(localImageFor("/uploads/0.png", 10)).toBeUndefined();
    expect(localImageFor("/uploads/1.png", 10)).toBe("file:///cache/1.png");
    expect(localImageFor("/uploads/200.png", 10)).toBe("file:///cache/200.png");
  });
});

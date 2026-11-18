// The pickers as the phone asks them (expo-image-picker and
// expo-document-picker are stubbed: src/testing/stubs).

import { beforeEach, describe, expect, it } from "vitest";

import { documentPicker } from "../../../testing/stubs/expoDocumentPicker";
import { imagePicker } from "../../../testing/stubs/expoImagePicker";
import { CAMERA_REFUSED, photoName, pickFiles } from "./pickers";

const NOON = new Date(2026, 10, 18, 14, 3, 22);

beforeEach(() => {
  imagePicker.reset();
  documentPicker.reset();
});

describe("the photo library", () => {
  it("asks for images only, as many as still fit, as they are", async () => {
    imagePicker.library = [
      { uri: "file:///cache/ImagePicker/a1.jpg", fileName: "IMG_0001.JPG", fileSize: 4096 },
      { uri: "file:///cache/ImagePicker/b2.png", fileName: null },
    ];
    expect(await pickFiles("library", 7)).toEqual([
      { uri: "file:///cache/ImagePicker/a1.jpg", name: "IMG_0001.JPG", size: 4096 },
      // No name from the picker: the file's own is used.
      { uri: "file:///cache/ImagePicker/b2.png" },
    ]);
    expect(imagePicker.launches).toEqual([{
      from: "library",
      options: { mediaTypes: ["images"], allowsMultipleSelection: true, selectionLimit: 7, quality: 1, exif: false },
    }]);
  });

  it("answers nothing when the user backs out, and never more than fit", async () => {
    expect(await pickFiles("library", 3)).toEqual([]);
    imagePicker.library = [1, 2, 3].map((index) => ({ uri: `file:///cache/${index}.png`, fileName: `${index}.png` }));
    expect((await pickFiles("library", 2)).map((file) => file.name)).toEqual(["1.png", "2.png"]);
  });

  it("opens no picker when nothing more fits", async () => {
    expect(await pickFiles("library", 0)).toEqual([]);
    expect(imagePicker.launches).toEqual([]);
  });
});

describe("the camera", () => {
  it("asks for the camera, then names the photo by when it was taken", async () => {
    imagePicker.camera = [{ uri: "file:///cache/ImagePicker/0c2d4e6f.jpg", fileName: "0c2d4e6f.jpg", fileSize: 2_000_000 }];
    expect(await pickFiles("camera", 4, NOON)).toEqual([
      { uri: "file:///cache/ImagePicker/0c2d4e6f.jpg", name: "Photo 2026-11-18 14.03.22.jpg", size: 2_000_000 },
    ]);
    expect(imagePicker.launches).toEqual([{ from: "camera", options: { mediaTypes: ["images"], quality: 1, exif: false } }]);
  });

  it("says so when the camera may not be used, and opens nothing", async () => {
    imagePicker.cameraGranted = false;
    await expect(pickFiles("camera", 4)).rejects.toThrow(CAMERA_REFUSED);
    expect(imagePicker.launches).toEqual([]);
  });

  it("answers nothing when the user backs out", async () => {
    expect(await pickFiles("camera", 4)).toEqual([]);
  });

  it("keeps the extension the camera gave its file", () => {
    expect(photoName("file:///cache/x.HEIC", NOON)).toBe("Photo 2026-11-18 14.03.22.heic");
    expect(photoName("content://media/external/images/123", NOON)).toBe("Photo 2026-11-18 14.03.22.jpg");
  });
});

describe("the file picker", () => {
  it("asks for a copy the phone can read, of any kind, several at once", async () => {
    documentPicker.files = [
      { uri: "file:///cache/DocumentPicker/layout.log", name: "layout.log", size: 18_432 },
      { uri: "file:///cache/DocumentPicker/notes.md", name: "notes.md" },
    ];
    expect(await pickFiles("file", 2)).toEqual([
      { uri: "file:///cache/DocumentPicker/layout.log", name: "layout.log", size: 18_432 },
      { uri: "file:///cache/DocumentPicker/notes.md", name: "notes.md" },
    ]);
    expect(documentPicker.opened).toEqual([{ type: "*/*", copyToCacheDirectory: true, multiple: true }]);
  });

  it("answers nothing when the user backs out", async () => {
    expect(await pickFiles("file", 2)).toEqual([]);
  });
});

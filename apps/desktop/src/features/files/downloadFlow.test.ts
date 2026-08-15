import { beforeEach, describe, expect, it, vi } from "vitest";
import { chooseDownloadDestination, revealDownloadLabel, suggestedDownloadName } from "./downloadFlow";

const save = vi.fn();
const invoke = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: (...args: unknown[]) => save(...args) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const intent = { path: "/work/report final.txt", kind: "file" as const };

describe("chooseDownloadDestination", () => {
  beforeEach(() => { save.mockReset(); invoke.mockReset(); });

  it("opens the panel once, on a name the backend has already made unique", async () => {
    invoke.mockResolvedValue("/Users/test/Downloads/report final (2).txt");
    save.mockResolvedValue("/Users/test/Downloads/report final (2).txt");

    expect(await chooseDownloadDestination(intent)).toBe("/Users/test/Downloads/report final (2).txt");
    expect(invoke).toHaveBeenCalledWith("suggest_download_destination", { fileName: "report final.txt" });
    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith({ title: "Save file", defaultPath: "/Users/test/Downloads/report final (2).txt" });
  });

  it("names a folder download's archive and titles the panel for it", async () => {
    invoke.mockResolvedValue("/Users/test/Downloads/archive.tar");
    save.mockResolvedValue("/Users/test/Downloads/archive.tar");

    await chooseDownloadDestination({ path: "/work/archive/", kind: "folder" });
    expect(invoke).toHaveBeenCalledWith("suggest_download_destination", { fileName: "archive.tar" });
    expect(save).toHaveBeenCalledWith(expect.objectContaining({ title: "Save folder archive" }));
  });

  it("treats a cancelled panel as a complete answer", async () => {
    invoke.mockResolvedValue("/Users/test/Downloads/report final.txt");
    save.mockResolvedValue(null);
    expect(await chooseDownloadDestination(intent)).toBeUndefined();
  });

  it("still opens the panel when the unique-name suggestion is unavailable", async () => {
    // A failure there costs a nicer default, never the download itself.
    invoke.mockRejectedValue(new Error("no Downloads directory"));
    save.mockResolvedValue("/elsewhere/report final.txt");

    expect(await chooseDownloadDestination(intent)).toBe("/elsewhere/report final.txt");
    expect(save).toHaveBeenCalledWith({ title: "Save file", defaultPath: "report final.txt" });
  });
});

describe("download naming", () => {
  it("uses a bounded basename and appends tar only for folder downloads", () => {
    expect(suggestedDownloadName(intent)).toBe("report final.txt");
    expect(suggestedDownloadName({ path: "/work/archive", kind: "folder" })).toBe("archive.tar");
    expect(suggestedDownloadName({ path: "/work/archive.tar", kind: "folder" })).toBe("archive.tar");
    expect(suggestedDownloadName({ path: "/", kind: "file" })).toBe("download");
  });

  it("only calls it Finder on macOS", () => {
    expect(revealDownloadLabel("mac")).toBe("Show in Finder");
    expect(revealDownloadLabel("linux")).toBe("Show in folder");
  });
});

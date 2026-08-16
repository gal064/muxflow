import { describe, expect, it } from "vitest";
import { reconcileDownloadStatus } from "./downloadStatus";
import type { TransferStatus } from "./types";

const completed: TransferStatus = {
  id: "download-1",
  scopeKey: "scope",
  path: "/repo/image.png",
  destination: "/tmp/image.png",
  kind: "file",
  state: "completed",
  outcome: "published",
  completedBytes: "12",
  totalBytes: "12",
  filesCompleted: "1",
};

describe("reconcileDownloadStatus", () => {
  it("announces a completed download by the local file the user now has", () => {
    // Not the remote source: that is the one path they cannot open, and it was
    // also wrong whenever the backend published under a different name.
    expect(reconcileDownloadStatus("Download running: /repo/image.png", { id: "download-1", path: "/repo/image.png", banner: "Download running: /repo/image.png" }, [completed]))
      .toEqual({
        status: "Download complete: /tmp/image.png",
        completion: { destination: "/tmp/image.png", message: "Download complete: /tmp/image.png" },
      });
  });

  it("keeps the source path and offers nothing to open when the download did not publish", () => {
    const active = { id: "download-1", path: "/repo/image.png", banner: "Download running: /repo/image.png" };
    for (const state of ["failed", "cancelled"] as const) {
      expect(reconcileDownloadStatus(active.banner, active, [{ ...completed, state, outcome: "notPublished" }]))
        .toEqual({ status: `Download ${state}: /repo/image.png` });
    }
    // A completion the backend never gave a destination for has nothing to act
    // on, so it must not offer an Open button pointed at nothing.
    expect(reconcileDownloadStatus(active.banner, active, [{ ...completed, destination: undefined }]))
      .toEqual({ status: "Download completed: /repo/image.png" });
  });

  it("does not overwrite unrelated newer status", () => {
    expect(reconcileDownloadStatus("File rename completed.", undefined, [completed])).toEqual({ status: "File rename completed." });
    expect(reconcileDownloadStatus("File rename completed.", { id: "download-1", path: "/repo/image.png", banner: "Download running: /repo/image.png" }, [completed]))
      .toEqual({ status: "File rename completed." });
  });

  it("does not let terminal history for the same source path complete a newer transfer", () => {
    const active = { id: "download-2", path: "/repo/image.png", banner: "Download running: /repo/image.png" };
    expect(reconcileDownloadStatus(active.banner, active, [completed]))
      .toEqual({ status: active.banner, active });
  });
});

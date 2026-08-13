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
  it("replaces an active banner when the same download reaches a terminal state", () => {
    expect(reconcileDownloadStatus("Download running: /repo/image.png", { id: "download-1", path: "/repo/image.png", banner: "Download running: /repo/image.png" }, [completed]))
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

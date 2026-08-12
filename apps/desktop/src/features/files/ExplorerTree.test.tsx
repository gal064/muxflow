import { renderToStaticMarkup } from "react-dom/server";
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { ExplorerTree } from "./ExplorerTree";
import type { ActiveRoot, DirectoryListing } from "./types";

const root: ActiveRoot = { token: "root-1", paneId: "%1", cwd: "/r", path: "/r", gitWorktree: true, revision: "1" };
const listing: DirectoryListing = {
  rootToken: "root-1", directory: "/r", revision: "2", overflowRecovery: false, complete: true,
  entries: [
    { path: "/r/.env", name: ".env", kind: "file", sizeBytes: "10", modifiedMillis: "1", executable: false, expandable: false },
    { path: "/r/ignored.log", name: "ignored.log", kind: "file", sizeBytes: "20", modifiedMillis: "1", executable: false, expandable: false },
    { path: "/r/.git", name: ".git", kind: "directory", sizeBytes: "0", modifiedMillis: "1", executable: false, expandable: false },
    { path: "/r/node_modules", name: "node_modules", kind: "directory", sizeBytes: "0", modifiedMillis: "1", executable: false, expandable: false },
    { path: "/r/link", name: "link", kind: "symlink", sizeBytes: "0", modifiedMillis: "1", executable: false, expandable: false, targetKind: "directory", symlinkTarget: "/outside" },
  ],
};

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("ExplorerTree", () => {
  it("shows dotfiles/ignored entries while protected and symlink directories stay collapsed", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} transfers={[]} disabled={false} error={undefined}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain(".env");
    expect(html).toContain("ignored.log");
    expect(html).toContain(".git");
    expect(html).toContain("node_modules");
    expect(html).toContain("link");
    expect(html).not.toContain('aria-expanded="false"');
    expect(html).toContain('role="tree"');
    expect(html).toContain('role="treeitem"');
    expect(html).toContain('aria-level="1"');
    expect(html).toContain('tabindex="0"');
    expect(html).toContain('aria-label="New file"');
  });

  it("exposes accessible cancellation for queued/running downloads but never terminal cancellation", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} disabled={false}
      transfers={[
        { id: "queued", scopeKey: "scope", path: "/r/queued", kind: "file", state: "queued", completedBytes: "0", totalBytes: "5368709120", filesCompleted: "0" },
        { id: "running", scopeKey: "scope", path: "/r/running", kind: "file", state: "running", completedBytes: "2", totalBytes: "10", filesCompleted: "0" },
        { id: "cancelled", scopeKey: "scope", path: "/r/cancelled", kind: "file", state: "cancelled", outcome: "notPublished", completedBytes: "2", filesCompleted: "0", cleanupStatus: "removed" },
      ]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain("Downloads");
    expect(html).toContain("Download /r/queued: Queued");
    expect(html).toContain("Download /r/running: Transferring");
    expect(html).toContain("5368709120");
    expect(html).toContain("Cancel download /r/queued");
    expect(html).toContain("Cancel download /r/running");
    expect(html).not.toContain("Cancel download /r/cancelled");
  });

  it("routes the accessible running-download button to its exact transfer id", async () => {
    const cancel = vi.fn(async () => undefined);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} disabled={false}
      transfers={[{ id: "running-id", scopeKey: "scope", path: "/r/running", kind: "file", state: "running", completedBytes: "2", totalBytes: "10", filesCompleted: "0" }]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={cancel} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "Cancel download /r/running" }).props.onClick(); });
    expect(cancel).toHaveBeenCalledWith("running-id");
    await act(async () => { renderer.unmount(); });
  });

  it("makes commit authoritative and renders unknown/cleanup outcomes as terminal alerts", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} disabled={false}
      transfers={[
        { id: "verifying", scopeKey: "scope", path: "/r/commit", kind: "file", state: "verifying", completedBytes: "12", totalBytes: "12", filesCompleted: "0" },
        { id: "unknown", scopeKey: "scope", path: "/r/unknown", kind: "file", state: "failed", outcome: "unknown", failureKind: "outcomeUnknown", completedBytes: "12", filesCompleted: "0", cleanupStatus: "failed", cleanupError: "permission denied" },
        { id: "timeout", scopeKey: "scope", path: "/r/timeout", kind: "file", state: "failed", outcome: "unknown", failureKind: "timeout", completedBytes: "12", filesCompleted: "0" },
        { id: "stale", scopeKey: "scope", path: "/r/stale", kind: "file", state: "failed", outcome: "unknown", failureKind: "staleScope", completedBytes: "12", filesCompleted: "0" },
      ]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain("Verifying and committing");
    expect(html).toContain("awaiting the authoritative backend outcome");
    expect(html).toContain("outcome is unknown");
    expect(html).toContain("Download timed out before an authoritative result arrived.");
    expect(html).toContain("Download stopped because the connection scope changed.");
    expect(html).toContain("Partial cleanup failed: permission denied");
    expect(html).not.toContain("Cancel download /r/commit");
  });
});

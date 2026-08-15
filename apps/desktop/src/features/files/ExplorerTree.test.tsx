// @vitest-environment jsdom
// jsdom, because opening a row action's dialog goes through `useModalDialog`,
// which manages real focus.
import { renderToStaticMarkup } from "react-dom/server";
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { rowCommandRegistry } from "../../commands/rowCommands";
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
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined}
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
    // The tree carries no resting controls at all now: creating, renaming,
    // downloading and refreshing are right-click items, not header buttons.
    expect(html).not.toContain('aria-label="New file"');
    expect(html).not.toContain('aria-label="Refresh Explorer"');
    expect(html).not.toContain("•••");
  });

  it("never changes the tree's height to say a directory is being re-read", () => {
    // The reported flicker: the "Loading…" row lives inside the scrolling box,
    // so showing it while rows are already up grew the content by a row and
    // shrank it again on every filesystem event — the row blinking on a short
    // listing, and macOS revealing and re-hiding the overlay scrollbars on a
    // long one. Whatever else a refresh does, it must not move that content.
    const shown = (props: { loading: Set<string>; requestedReads: number; listings: Map<string, DirectoryListing> }) =>
      renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" expanded={new Set(["/r"])} transfers={[]} disabled={false} error={undefined}
        onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} {...props} />);
    const listings = new Map([["/r", listing]]);
    const resting = shown({ loading: new Set(), requestedReads: 0, listings });
    const refreshing = shown({ loading: new Set(["/r"]), requestedReads: 0, listings });
    expect(resting).not.toContain("Loading…");
    expect(refreshing, "a re-read put a row back inside the scrolling tree").not.toContain("Loading…");
    // The tree's own markup is untouched by the refresh; only the busy flag moves.
    expect(refreshing.replace(' aria-busy="true"', "")).toBe(resting);
    expect(refreshing).toContain('aria-busy="true"');

    // A directory that has never answered is the one case that still says so,
    // because then there is nothing for the wait to be in front of.
    const first = shown({ loading: new Set(["/r"]), requestedReads: 0, listings: new Map() });
    expect(first).toContain("Loading…");
    // And an empty directory does not swap between two lines on every re-read.
    const empty = new Map([["/r", { ...listing, entries: [] }]]);
    expect(shown({ loading: new Set(["/r"]), requestedReads: 0, listings: empty })).toContain("This directory is empty.");
    expect(shown({ loading: new Set(), requestedReads: 0, listings: empty })).toContain("This directory is empty.");

    // A refresh the user asked for is the one wait that is visible, and it is
    // drawn in the header — outside the scrolling box, so it cannot flicker it.
    const requested = shown({ loading: new Set(["/r"]), requestedReads: 1, listings });
    expect(requested).toContain("Refreshing…");
    expect(requested.slice(0, requested.indexOf('role="tree"'))).toContain("Refreshing…");
  });

  it("exposes accessible cancellation for queued/running downloads but never terminal cancellation", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} disabled={false}
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
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} disabled={false}
      transfers={[{ id: "running-id", scopeKey: "scope", path: "/r/running", kind: "file", state: "running", completedBytes: "2", totalBytes: "10", filesCompleted: "0" }]}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={cancel} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "Cancel download /r/running" }).props.onClick(); });
    expect(cancel).toHaveBeenCalledWith("running-id");
    await act(async () => { renderer.unmount(); });
  });

  it("reaches every row action from the command registry, on the row the tree has focus on", async () => {
    // The palette's half of "a removed button becomes a palette command, a
    // context-menu item and a shortcut". Nothing here goes through the menu:
    // this is the route ⌘K takes.
    const onDownload = vi.fn(async () => undefined);
    const onRefresh = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled={false} error={undefined}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={onDownload} onCancelTransfer={vi.fn()} onRefresh={onRefresh} onLoadMore={vi.fn()} />); });

    expect(rowCommandRegistry.available()).toEqual([
      "files.open", "files.download", "files.rename", "files.move", "files.duplicate", "files.delete",
      "files.newFile", "files.newFolder", "files.refresh",
    ]);
    // Clicking a row is pointing at it. macOS WebKit does not focus a button on
    // click, so a tree that only listened for focus went on offering actions for
    // whichever row the keyboard last visited — measured on the packaged app,
    // where clicking README.md left `.git` as the palette's subject.
    const gitDirectory = renderer.root.findAllByProps({ "data-tree-index": 2 })[0];
    await act(async () => { gitDirectory.props.onPointerDown(); });
    expect(rowCommandRegistry.available()).not.toContain("files.open");
    const dotEnv = renderer.root.findAllByProps({ "data-tree-index": 0 })[0];
    await act(async () => { dotEnv.props.onPointerDown(); });
    expect(rowCommandRegistry.available()).toContain("files.open");
    // Row 0 is `.env`; the tree's own focus cursor is what "selected" means.
    await act(async () => { rowCommandRegistry.run("files.download"); });
    expect(onDownload).toHaveBeenCalledWith({ path: "/r/.env", kind: "file", collision: "fail" });
    await act(async () => { rowCommandRegistry.run("files.refresh"); });
    expect(onRefresh).toHaveBeenCalled();
    await act(async () => { rowCommandRegistry.run("files.rename"); });
    expect(JSON.stringify(renderer.toJSON())).toContain("Rename");

    await act(async () => { renderer.unmount(); });
    // A closed panel offers nothing; the palette must not hold a row nobody
    // can see.
    expect(rowCommandRegistry.available()).toEqual([]);
    expect(rowCommandRegistry.run("files.rename")).toEqual({ ran: false });
  });

  it("withholds the mutating row actions while the host is read-only", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map([["/r", listing]])} expanded={new Set(["/r"])} loading={new Set()} requestedReads={0} transfers={[]} disabled error={undefined}
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />); });
    expect(rowCommandRegistry.available()).toEqual(["files.open", "files.download", "files.refresh"]);
    await act(async () => { renderer.unmount(); });
  });

  it("summarizes a host rejection and keeps the diagnostic behind a disclosure", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} transfers={[]} disabled={false}
      error="file_mutation_rejected: File name too long (os error 63)"
      onToggle={vi.fn()} onOpen={vi.fn()} onMutate={vi.fn()} onDownload={vi.fn()} onCancelTransfer={vi.fn()} onRefresh={vi.fn()} onLoadMore={vi.fn()} />);
    expect(html).toContain("That name is longer than this filesystem allows.");
    expect(html).toContain("<details");
    expect(html).toContain("os error 63");
  });

  it("makes commit authoritative and renders unknown/cleanup outcomes as terminal alerts", () => {
    const html = renderToStaticMarkup(<ExplorerTree root={root} scopeIdentity="scope" listings={new Map()} expanded={new Set()} loading={new Set()} requestedReads={0} disabled={false}
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

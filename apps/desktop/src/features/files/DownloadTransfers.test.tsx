// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { DownloadTransfers } from "./DownloadTransfers";
import { revealDownloadLabel } from "./downloadFlow";
import type { TransferStatus } from "./types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const published = "/Users/test/Downloads/report (1).pdf";
const transfers: TransferStatus[] = [
  { id: "done", scopeKey: "scope", path: "/r/report.pdf", destination: published, kind: "file", state: "completed", outcome: "published", completedBytes: "9", totalBytes: "9", filesCompleted: "1" },
  { id: "running", scopeKey: "scope", path: "/r/running", kind: "file", state: "running", completedBytes: "2", totalBytes: "10", filesCompleted: "0" },
  // Terminal, but nothing landed locally: there is no file to point at.
  { id: "failed", scopeKey: "scope", path: "/r/failed", kind: "file", state: "failed", outcome: "notPublished", completedBytes: "2", filesCompleted: "0" },
  // Published, then its scope went stale. The file exists and this is exactly
  // when the user most needs to find it, so the actions must be offered.
  { id: "stale", scopeKey: "scope", path: "/r/stale.bin", destination: "/Users/test/Downloads/stale.bin", kind: "file", state: "failed", outcome: "published", failureKind: "staleScope", completedBytes: "9", filesCompleted: "1" },
];

const render = async (
  rows: readonly TransferStatus[] = transfers,
  onClearFinishedTransfers = vi.fn(),
) => {
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<DownloadTransfers
    onCancelTransfer={vi.fn()}
    onClearFinishedTransfers={onClearFinishedTransfers}
    transfers={rows}
  />); });
  return renderer;
};

describe("DownloadTransfers", () => {
  beforeEach(() => { invoke.mockClear(); invoke.mockImplementation(() => Promise.resolve()); });

  it("offers Open and reveal only on a download that actually published a local file", async () => {
    const renderer = await render();
    const markup = JSON.stringify(renderer.toJSON());
    expect(markup).toContain(`Open ${published}`);
    expect(markup).toContain(revealDownloadLabel());
    expect(markup).not.toContain("Open /r/running");
    expect(markup).not.toContain("Open /r/failed");
    expect(markup, "a published file with a stale scope had nothing to open")
      .toContain("Open /Users/test/Downloads/stale.bin");

    // The buttons name the *published* path, not the remote source: a renamed
    // destination is the file the user actually has, and it is also the only
    // path the backend will agree to open.
    await act(async () => { renderer.root.findByProps({ "aria-label": `Open ${published}` }).props.onClick(); });
    expect(invoke).toHaveBeenCalledWith("open_download", { path: published });
    await act(async () => { renderer.root.findByProps({ "aria-label": `${revealDownloadLabel()}: ${published}` }).props.onClick(); });
    expect(invoke).toHaveBeenCalledWith("reveal_download", { path: published });
    await act(async () => { renderer.unmount(); });
  });

  it("reports a refused open on the row it happened to, not in the app's status channel", async () => {
    invoke.mockImplementation(() => Promise.reject(new Error("only a download this session completed can be opened")));
    const renderer = await render();
    await act(async () => { renderer.root.findByProps({ "aria-label": `Open ${published}` }).props.onClick(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("only a download this session completed can be opened");
    await act(async () => { renderer.unmount(); });
  });

  it("clears a refusal once the same row opens successfully", async () => {
    // A row that has recovered must not keep showing the error it recovered
    // from — the previous shape only ever set the message, never unset it.
    invoke.mockImplementation(() => Promise.reject(new Error("refused")));
    const renderer = await render();
    await act(async () => { renderer.root.findByProps({ "aria-label": `Open ${published}` }).props.onClick(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("refused");

    invoke.mockImplementation(() => Promise.resolve());
    await act(async () => { renderer.root.findByProps({ "aria-label": `Open ${published}` }).props.onClick(); });
    expect(JSON.stringify(renderer.toJSON())).not.toContain("refused");
    await act(async () => { renderer.unmount(); });
  });

  it("draws nothing at all when there are no transfers", async () => {
    const renderer = await render([]);
    expect(renderer.toJSON()).toBeNull();
    await act(async () => { renderer.unmount(); });
  });

  it("keeps cancellation on the states that can still be cancelled", async () => {
    const renderer = await render();
    const markup = JSON.stringify(renderer.toJSON());
    expect(markup).toContain("Cancel download /r/running");
    expect(markup).not.toContain("Cancel download /r/report.pdf");
    expect(markup).not.toContain("Cancel download /r/failed");
    await act(async () => { renderer.unmount(); });
  });

  it("offers one clear action when finished downloads exist and routes it without touching active rows", async () => {
    const clearFinished = vi.fn();
    const renderer = await render(transfers, clearFinished);
    const button = renderer.root.findByProps({ "aria-label": "Clear finished downloads" });
    expect(button.props.children).toBe("Clear all");
    await act(async () => { button.props.onClick(); });
    expect(clearFinished).toHaveBeenCalledTimes(1);
    await act(async () => { renderer.unmount(); });

    const activeOnly = await render([transfers[1]]);
    expect(activeOnly.root.findAllByProps({ "aria-label": "Clear finished downloads" })).toHaveLength(0);
    await act(async () => { activeOnly.unmount(); });
  });
});

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
];

const render = async (rows: readonly TransferStatus[] = transfers) => {
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<DownloadTransfers onCancelTransfer={vi.fn()} transfers={rows} />); });
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
});

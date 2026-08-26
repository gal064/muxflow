import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ActiveRoot,
  FileWorkspaceClient,
  FileWorkspaceScope,
  TransferStatus,
} from "../features/files/types";
import { defaultAppState } from "../features/shell/types";
import { useAppFileActions } from "./useAppFileActions";

const picker = vi.hoisted(() => ({ choose: vi.fn() }));
const journal = vi.hoisted(() => ({ record: vi.fn() }));
vi.mock("../features/files/downloadFlow", async (importOriginal) => ({
  ...await importOriginal<typeof import("../features/files/downloadFlow")>(),
  chooseDownloadDestination: picker.choose,
}));
vi.mock("../diagnostics/incidents", () => ({ recordIncident: journal.record }));

const root: ActiveRoot = {
  token: "root-a", paneId: "%1", cwd: "/work", path: "/work", gitWorktree: false, revision: "1",
};
const scope = (clientId: string): FileWorkspaceScope => ({
  clientId,
  hostProfileId: clientId,
  serverIdentity: `server-${clientId}`,
  generation: 1,
  terminalEpoch: 1,
  sessionId: "$1",
  paneId: "%1",
});

let actions!: ReturnType<typeof useAppFileActions>;
function Harness(props: { client: FileWorkspaceClient; scope?: FileWorkspaceScope; root?: ActiveRoot }) {
  actions = useAppFileActions({
    canMutate: true,
    client: props.client,
    currentHostProfileId: props.scope?.hostProfileId ?? "local",
    recordTransfer: vi.fn(),
    refreshDirectory: vi.fn(),
    root: props.root,
    scope: props.scope,
    setActiveDownloadStatus: vi.fn(),
    setAppState: vi.fn((update) => typeof update === "function" && update(defaultAppState)),
    setStatus: vi.fn(),
  });
  return null;
}

describe("useAppFileActions", () => {
  beforeEach(() => {
    journal.record.mockClear();
    picker.choose.mockReset();
  });

  it("records the exact predicate when a valid file tab differs from the live Explorer root", async () => {
    const client = { startDownload: vi.fn(), cancelTransfer: vi.fn() } as unknown as FileWorkspaceClient;
    const currentRoot = { ...root, token: "root-b", path: "/other", cwd: "/other" };
    await act(async () => { create(<Harness client={client} scope={scope("a")} root={currentRoot} />); });

    await act(async () => {
      await actions.startDownloadFlow({ path: "/work/report", kind: "file" }, root, "fileSurface");
    });

    expect(picker.choose).not.toHaveBeenCalled();
    expect(journal.record).toHaveBeenCalledWith("download.workspaceRejected", expect.objectContaining({
      origin: "fileSurface", stage: "prePicker", currentScopePresent: true, currentRootPresent: true,
      scopeKeyMatch: true, rootMatch: false, rootTokenMatch: false, rootPathMatch: false, rootPaneMatch: true,
    }));
  });

  it("distinguishes a restored tab rendered before the live root is reacquired", async () => {
    const client = { startDownload: vi.fn(), cancelTransfer: vi.fn() } as unknown as FileWorkspaceClient;
    await act(async () => { create(<Harness client={client} scope={scope("a")} root={undefined} />); });

    await act(async () => {
      await actions.startDownloadFlow({ path: "/work/report", kind: "file" }, root, "fileSurface");
    });

    expect(journal.record).toHaveBeenCalledWith("download.workspaceRejected", expect.objectContaining({
      stage: "prePicker", currentScopePresent: true, currentRootPresent: false,
      scopeKeyMatch: true, rootMatch: false, rootTokenMatch: false, rootPathMatch: false, rootPaneMatch: false,
    }));
  });

  it("does not start a download after the save panel outlives its host scope", async () => {
    let resolvePicker!: (value: { destination: string; panelConfirmed: boolean }) => void;
    picker.choose.mockReturnValueOnce(new Promise((resolve) => { resolvePicker = resolve; }));
    const client = {
      startDownload: vi.fn(),
      cancelTransfer: vi.fn(),
    } as unknown as FileWorkspaceClient;
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<Harness client={client} scope={scope("a")} root={root} />); });

    let flow!: Promise<void>;
    act(() => { flow = actions.startDownloadFlow({ path: "/work/report", kind: "file" }, root, "fileSurface"); });
    await act(async () => { renderer.update(<Harness client={client} scope={scope("b")} root={root} />); });
    await act(async () => { resolvePicker({ destination: "/tmp/report", panelConfirmed: false }); await flow; });

    expect(client.startDownload).not.toHaveBeenCalled();
  });

  it("cancels and suppresses a completion published after the scope changes", async () => {
    picker.choose.mockResolvedValueOnce({ destination: "/tmp/report", panelConfirmed: false });
    let resolveTransfer!: (value: TransferStatus) => void;
    const startDownload = vi.fn(() => new Promise<TransferStatus>((resolve) => { resolveTransfer = resolve; }));
    const cancelTransfer = vi.fn().mockResolvedValue(undefined);
    const client = { startDownload, cancelTransfer } as unknown as FileWorkspaceClient;
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<Harness client={client} scope={scope("a")} root={root} />); });

    let flow!: Promise<void>;
    await act(async () => {
      flow = actions.startDownloadFlow({ path: "/work/report", kind: "file" }, root, "fileSurface");
      await Promise.resolve();
    });
    await act(async () => { renderer.update(<Harness client={client} scope={scope("b")} root={root} />); });
    await act(async () => {
      resolveTransfer({
        id: "transfer-1", scopeKey: "old", path: "/work/report", destination: "/tmp/report",
        kind: "file", state: "queued", completedBytes: "0", filesCompleted: "0",
      });
      await flow;
    });

    expect(cancelTransfer).toHaveBeenCalledWith(expect.objectContaining({ clientId: "a" }), "transfer-1");
  });
});

// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import type { GitDiff, GitStatusSnapshot, GitWorkspaceClient } from "./types";
import { GitDiffSurface } from "./GitDiffSurface";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
vi.mock("@monaco-editor/react", () => ({ DiffEditor: (props: { original: string; modified: string }) => <div data-modified={props.modified} data-original={props.original}>diff editor</div> }));
vi.mock("../files/monaco", () => ({ ADE_MONACO_THEME: "ade-dark" }));

describe("GitDiffSurface", () => {
  it("renders a read-only Monaco diff and exposes complete-hunk actions", async () => {
    const client = mockClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(renderer.root.findByProps({ "data-original": "old\n" }).props["data-modified"]).toBe("new\n");
    expect(renderer.root.findAllByProps({ "aria-label": "Complete hunk actions" })).toHaveLength(1);
    expect(JSON.stringify(renderer.toJSON())).toContain('"Hunk ","2"');
    await act(async () => { renderer.unmount(); });
  });

  it("performs no destructive call when complete-hunk discard is cancelled", async () => {
    const client = mockClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const discard = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard…" && button.parent?.props.children?.[0]?.props?.children === "Hunk 1");
    await act(async () => { discard?.props.onClick(); });
    const cancel = renderer.root.findAllByType("button").find((button) => button.props.children === "Cancel");
    await act(async () => { cancel?.props.onClick(); await settle(); });
    expect(client.prepareDiscard).not.toHaveBeenCalled();
    expect(client.mutate).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
  });

  it("removes stale actionable diff content while a post-mutation refresh is pending", async () => {
    const client = mockClient();
    let resolveRefresh!: (value: GitStatusSnapshot) => void;
    vi.mocked(client.status)
      .mockResolvedValueOnce(status)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveRefresh = resolve; }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const stage = renderer.root.findAllByType("button").find((button) => button.props.children === "Stage file");
    await act(async () => { stage?.props.onClick(); await settle(); });
    expect(renderer.root.findAllByProps({ "data-original": "old\n" })).toHaveLength(0);
    expect(JSON.stringify(renderer.toJSON())).toContain("Loading Git diff");
    await act(async () => { resolveRefresh(status); await settle(); });
    await act(async () => { renderer.unmount(); });
  });

  it("does not resurrect an unstaged diff after the mutation moves the file to staged changes", async () => {
    const client = mockClient();
    const stagedStatus = {
      ...status,
      generation: "8",
      entries: [{ ...status.entries[0], indexKind: "added" as const, worktreeKind: "none" as const, indexStatus: "A", worktreeStatus: "." }],
    };
    vi.mocked(client.status).mockResolvedValueOnce(status).mockResolvedValueOnce(stagedStatus);
    vi.mocked(client.mutate).mockResolvedValueOnce({
      exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied", status: stagedStatus,
    });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const stage = renderer.root.findAllByType("button").find((button) => button.props.children === "Stage file");
    await act(async () => { stage?.props.onClick(); await settle(); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("no longer has unstaged changes");
    expect(renderer.root.findAllByType("button").filter((button) => button.props.children === "Stage file")).toHaveLength(0);
    expect(client.diff).toHaveBeenCalledTimes(1);
    await act(async () => { renderer.unmount(); });
  });

  it("mints a one-time host token only after confirmation and binds it to the stale-guarded hunk", async () => {
    const client = mockClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const hunkArea = renderer.root.findByProps({ "aria-label": "Complete hunk actions" });
    const discard = hunkArea.findAllByType("button").find((button) => button.props.children === "Discard…");
    await act(async () => { discard?.props.onClick(); });
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); await settle(); });
    expect(client.prepareDiscard).toHaveBeenCalledWith(scope, expect.objectContaining({ path: "/repo", token: "root" }), "repo", expect.objectContaining({
      kind: "discardHunk", hunkIndex: 0, path: "YQ==", expectedStatusGeneration: "7", expectedSourceGeneration: "diff-source",
    }));
    expect(client.mutate).toHaveBeenCalledWith(scope, expect.objectContaining({ path: "/repo", token: "root" }), "repo", expect.objectContaining({ confirmationToken: "one-time", hunkIndex: 0 }));
    await act(async () => { renderer.unmount(); });
  });

  it("fails closed when a persisted tab resolves to a different repository", async () => {
    const client = mockClient();
    vi.mocked(client.status).mockResolvedValueOnce({ ...status, repository: { ...status.repository, id: "other" } });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("different repository");
    expect(client.diff).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
  });

  it("presents binary changes safely without constructing a text diff", async () => {
    const client = mockClient();
    vi.mocked(client.diff).mockResolvedValueOnce({ ...diff, binary: true, oldContent: undefined, newContent: undefined });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("Binary changes cannot be displayed");
    expect(renderer.root.findAllByProps({ "data-original": "old\n" })).toHaveLength(0);
    await act(async () => { renderer.unmount(); });
  });

  it("owns an origin-root watch and reloads after the active pane leaves its repository", async () => {
    const client = mockClient();
    let listener: Parameters<GitWorkspaceClient["subscribe"]>[0] | undefined;
    vi.mocked(client.subscribe).mockImplementation((next) => { listener = next; return () => undefined; });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} activeRoot={{ ...root, path: "/other", token: "other" }} />); await settle(); });
    expect(client.diff).toHaveBeenCalledTimes(1);
    const changed = { ...status, generation: "8", sourceGeneration: "changed" };
    vi.mocked(client.status).mockResolvedValue(changed);
    await act(async () => { listener?.({ kind: "status", rootToken: "root", watchId: "diff-watch", status: changed }); await settle(); await settle(); });
    expect(client.diff).toHaveBeenCalledTimes(2);
    await act(async () => { renderer.unmount(); });
  });

  it("reconciles a newer watch event delivered before the bootstrap promise resolves", async () => {
    const client = mockClient();
    let listener: Parameters<GitWorkspaceClient["subscribe"]>[0] | undefined;
    let resolveWatch!: (lease: Awaited<ReturnType<GitWorkspaceClient["watch"]>>) => void;
    vi.mocked(client.subscribe).mockImplementation((next) => { listener = next; return () => undefined; });
    vi.mocked(client.watch).mockImplementation(() => new Promise((resolve) => { resolveWatch = resolve; }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const changed = { ...status, generation: "8", sourceGeneration: "changed" };
    vi.mocked(client.status).mockResolvedValue(changed);
    await act(async () => {
      listener?.({ kind: "status", rootToken: "root", watchId: "diff-watch", status: changed });
      resolveWatch({ watchId: "diff-watch", rootToken: "root", connectionEpoch: 1, status, release: vi.fn() });
      await settle(); await settle();
    });
    expect(client.diff).toHaveBeenCalledTimes(2);
    await act(async () => { renderer.unmount(); });
  });

  it("confirms complete-hunk discard from a staged diff", async () => {
    const client = mockClient();
    const stagedStatus = { ...status, entries: [{ ...status.entries[0], indexKind: "modified" as const, worktreeKind: "none" as const, indexStatus: "M", worktreeStatus: "." }] };
    vi.mocked(client.status).mockResolvedValue(stagedStatus);
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: stagedStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue({ ...diff, target: "staged" });
    const stagedTab = { ...tab, gitTarget: "staged" as const };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} tab={stagedTab} />); await settle(); });
    const hunkArea = renderer.root.findByProps({ "aria-label": "Complete hunk actions" });
    const discard = hunkArea.findAllByType("button").find((button) => button.props.children === "Discard…");
    await act(async () => { discard?.props.onClick(); });
    expect(client.prepareDiscard).not.toHaveBeenCalled();
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); });
    expect(client.prepareDiscard).toHaveBeenCalledWith(scope, expect.objectContaining({ path: "/repo", token: "root" }), "repo", expect.objectContaining({ kind: "discardHunk", target: "staged", hunkIndex: 0 }));
    await act(async () => { renderer.unmount(); });
  });

  it("keeps submodule diffs visible but disables every mutation", async () => {
    const client = mockClient();
    const submoduleStatus = { ...status, entries: [{ ...status.entries[0], submodule: true, submoduleState: "S.M." }] };
    vi.mocked(client.status).mockResolvedValue(submoduleStatus);
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: submoduleStatus, release: vi.fn() }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("Submodule pointer changes are read-only in v1");
    for (const button of renderer.root.findAllByType("button").filter((candidate) => ["Stage file", "Discard file…", "Stage", "Discard…"].includes(String(candidate.props.children)))) {
      expect(button.props.disabled).toBe(true);
    }
    await act(async () => { renderer.unmount(); });
  });

  it("preserves rename provenance for staged diff content and whole-file actions while omitting hunks", async () => {
    const client = mockClient();
    const renameStatus = { ...status, entries: [{ ...status.entries[0], indexKind: "renamed" as const, indexStatus: "R", originalPath: "b2xk", displayOriginalPath: "old" }] };
    const renameDiff = { ...diff, target: "staged" as const, originalPath: "b2xk", oldContent: new TextEncoder().encode("old path\n"), newContent: new TextEncoder().encode("new path\n") };
    vi.mocked(client.status).mockResolvedValue(renameStatus);
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: renameStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue(renameDiff);
    const renameTab = { ...tab, gitTarget: "staged" as const, gitOriginalPath: "b2xk" };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} tab={renameTab} />); await settle(); });
    expect(client.diff).toHaveBeenCalledWith(scope, expect.objectContaining({ token: "root" }), "repo", "YQ==", "b2xk", "staged", "7", expect.any(AbortSignal));
    expect(renderer.root.findByProps({ "data-original": "old path\n" }).props["data-modified"]).toBe("new path\n");
    expect(renderer.root.findAllByProps({ "aria-label": "Complete hunk actions" })).toHaveLength(0);
    const unstage = renderer.root.findAllByType("button").find((button) => button.props.children === "Unstage file");
    await act(async () => { unstage?.props.onClick(); await settle(); });
    expect(client.mutate).toHaveBeenCalledWith(scope, expect.objectContaining({ token: "root" }), "repo", expect.objectContaining({ kind: "unstageFile", path: "YQ==", originalPath: "b2xk" }));
    await act(async () => { renderer.unmount(); });
  });

  it("preserves copy provenance for an unstaged diff and whole-file action", async () => {
    const client = mockClient();
    const copyStatus = { ...status, entries: [{ ...status.entries[0], worktreeKind: "copied" as const, worktreeStatus: "C", originalPath: "b2xk", displayOriginalPath: "old" }] };
    vi.mocked(client.status).mockResolvedValue(copyStatus);
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: copyStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue({ ...diff, originalPath: "b2xk" });
    const copyTab = { ...tab, gitOriginalPath: "b2xk" };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} tab={copyTab} />); await settle(); });
    const stage = renderer.root.findAllByType("button").find((button) => button.props.children === "Stage file");
    await act(async () => { stage?.props.onClick(); await settle(); });
    expect(client.mutate).toHaveBeenCalledWith(scope, expect.objectContaining({ token: "root" }), "repo", expect.objectContaining({ kind: "stageFile", originalPath: "b2xk" }));
    expect(renderer.root.findAllByProps({ "aria-label": "Complete hunk actions" })).toHaveLength(0);
    await act(async () => { renderer.unmount(); });
  });
});

const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 1, sessionId: "$1", paneId: "%1" };
const root: ActiveRoot = { token: "root", path: "/repo", cwd: "/repo", paneId: "%1", gitWorktree: true, revision: "1" };
const tab: AppOwnedTab = { id: "tab", hostProfileId: "local", serverIdentity: "s", sessionId: "$1", sessionName: "one", kind: "gitDiff", resource: "unstaged:a", title: "a (unstaged)", order: 0, rootPath: "/repo", rootToken: "root", gitRepositoryId: "repo", gitPath: "YQ==", gitTarget: "unstaged", gitStatusGeneration: "7", gitSourceGeneration: "diff-source" };
const status: GitStatusSnapshot = { repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "7", sourceGeneration: "status-source", authoritative: true, entries: [{ path: "YQ==", displayPath: "a", indexKind: "none", worktreeKind: "modified", indexStatus: ".", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false }] };
const diff: GitDiff = { repository: status.repository, target: "unstaged", path: "YQ==", displayPath: "a", oldContent: new TextEncoder().encode("old\n"), newContent: new TextEncoder().encode("new\n"), patch: new Uint8Array(), sourceGeneration: "diff-source", binary: false, tooLarge: false, oldMissing: false, newMissing: false, hunkCount: 2 };
function mockClient(): GitWorkspaceClient { return { status: vi.fn(async () => status), watch: vi.fn(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status, release: vi.fn() })), diff: vi.fn(async () => diff), prepareDiscard: vi.fn(async () => "one-time"), mutate: vi.fn(async () => ({ exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied" as const, status })), commit: vi.fn(), subscribe: vi.fn(() => () => undefined) }; }
function props(client: GitWorkspaceClient) { return { tab, scope, activeRoot: root, client, canWrite: true, onMessage: vi.fn(), onStatus: vi.fn() }; }
async function settle() { await Promise.resolve(); await new Promise((resolve) => setTimeout(resolve, 0)); }

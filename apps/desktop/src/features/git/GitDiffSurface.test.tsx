// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import { GitRepositoryStore } from "./repositoryStore";
import type { GitDiff, GitStatusSnapshot, GitWorkspaceClient } from "./types";
import { GitDiffSurface } from "./GitDiffSurface";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
vi.mock("@monaco-editor/react", () => ({ DiffEditor: (props: { original: string; modified: string }) => <div data-modified={props.modified} data-original={props.original}>diff editor</div> }));
vi.mock("../files/monaco", () => ({ ADE_MONACO_THEME: "ade-dark" }));

describe("GitDiffSurface", () => {
  it("opens a matching diff in one request, with no prerequisite status round trip", async () => {
    const client = mockClient();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(renderer.root.findByProps({ "data-original": "old\n" }).props["data-modified"]).toBe("new\n");
    expect(renderer.root.findAllByProps({ "aria-label": "Complete hunk actions" })).toHaveLength(1);
    expect(JSON.stringify(renderer.toJSON())).toContain('"Hunk ","2"');
    expect(client.diff).toHaveBeenCalledTimes(1);
    expect(client.status).not.toHaveBeenCalled();
    expect(client.watch).toHaveBeenCalledTimes(1);
    await act(async () => { renderer.unmount(); });
  });

  it("joins an observation another consumer already holds instead of watching again", async () => {
    const client = mockClient();
    const repositories = new GitRepositoryStore(client);
    const sidebar = repositories.acquire(scope, root);
    await act(async () => { await settle(); });
    expect(client.watch).toHaveBeenCalledTimes(1);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} repositories={repositories} />); await settle(); });
    expect(client.watch).toHaveBeenCalledTimes(1);
    expect(client.status).not.toHaveBeenCalled();
    expect(client.diff).toHaveBeenCalledTimes(1);
    await act(async () => { renderer.unmount(); });
    sidebar.release();
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

  it("removes stale actionable diff content while a post-mutation reload is pending", async () => {
    const client = mockClient();
    const stillChanged = { ...status, generation: "8", sourceGeneration: "after-stage" };
    vi.mocked(client.mutate).mockResolvedValueOnce({
      exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied", status: stillChanged,
    });
    let resolveReload!: (value: { diff: GitDiff; status: GitStatusSnapshot }) => void;
    vi.mocked(client.diff)
      .mockResolvedValueOnce({ diff, status })
      .mockImplementationOnce(() => new Promise((resolve) => { resolveReload = resolve; }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const stage = renderer.root.findAllByType("button").find((button) => button.props.children === "Stage file");
    await act(async () => { stage?.props.onClick(); await settle(); });
    expect(renderer.root.findAllByProps({ "data-original": "old\n" })).toHaveLength(0);
    expect(JSON.stringify(renderer.toJSON())).toContain("Loading Git diff");
    await act(async () => { resolveReload({ diff, status: stillChanged }); await settle(); });
    expect(renderer.root.findAllByProps({ "data-original": "old\n" })).toHaveLength(1);
    await act(async () => { renderer.unmount(); });
  });

  it("performs one request and one remaining-diff read for a mutation that clears the file", async () => {
    const client = mockClient();
    const stagedStatus = {
      ...status,
      generation: "8",
      sourceGeneration: "staged",
      entries: [{ ...status.entries[0], indexKind: "added" as const, worktreeKind: "none" as const, indexStatus: "A", worktreeStatus: "." }],
    };
    vi.mocked(client.mutate).mockResolvedValueOnce({
      exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied", status: stagedStatus,
    });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const stage = renderer.root.findAllByType("button").find((button) => button.props.children === "Stage file");
    await act(async () => { stage?.props.onClick(); await settle(); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("no longer has unstaged changes");
    expect(renderer.root.findAllByType("button").filter((button) => button.props.children === "Stage file")).toHaveLength(0);
    expect(client.mutate).toHaveBeenCalledTimes(1);
    // The mutation's own status showed the file has no unstaged change left, so
    // the reload needs no request at all.
    expect(client.diff).toHaveBeenCalledTimes(1);
    expect(client.status).not.toHaveBeenCalled();
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

  it("fails closed without a request when a persisted tab resolves to a different repository", async () => {
    const client = mockClient();
    const foreign = { ...status, repository: { ...status.repository, id: "other" } };
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: foreign, release: vi.fn() }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("different repository");
    expect(client.diff).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
  });

  it("recovers a file the shared observation wrongly believes is unchanged", async () => {
    const client = mockClient();
    // The shared observation says this file has no unstaged change, so the tab
    // shows its empty state and no diff request is worth sending.
    const clean = {
      ...status,
      generation: "6",
      sourceGeneration: "clean",
      entries: [{ ...status.entries[0], worktreeKind: "none" as const, worktreeStatus: "." }],
    };
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: clean, release: vi.fn() }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("no longer has unstaged changes");
    expect(client.diff).not.toHaveBeenCalled();

    // Retry is the person asking again. It must reach the host, not re-read the
    // very snapshot that produced the empty state.
    vi.mocked(client.status).mockResolvedValue(status);
    const retry = renderer.root.findAllByType("button").find((button) => button.props.children === "Retry");
    await act(async () => { retry?.props.onClick(); await settle(); await settle(); });
    expect(client.status).toHaveBeenCalledTimes(1);
    expect(client.diff).toHaveBeenCalledTimes(1);
    expect(renderer.root.findByProps({ "data-original": "old\n" }).props["data-modified"]).toBe("new\n");
    await act(async () => { renderer.unmount(); });
  });

  it("surfaces a failed shared bootstrap instead of loading forever", async () => {
    const client = mockClient();
    vi.mocked(client.watch).mockRejectedValue(new Error("watch refused: not a worktree"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    const rendered = JSON.stringify(renderer.toJSON());
    expect(rendered).toContain("watch refused");
    expect(rendered).not.toContain("Loading Git diff");
    // And the way out is offered, not just the reason.
    expect(renderer.root.findAllByType("button").some((button) => button.props.children === "Retry")).toBe(true);
    await act(async () => { renderer.unmount(); });
  });

  it("follows the tab to another file without reloading the previous one", async () => {
    const client = mockClient();
    const other = { ...status.entries[0], path: "Yg==", displayPath: "b" };
    const both = { ...status, entries: [status.entries[0], other] };
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: both, release: vi.fn() }));
    vi.mocked(client.diff).mockImplementation(async (_scope, _root, _id, path) => ({ diff: { ...diff, path }, status: both }));
    let listener: Parameters<GitWorkspaceClient["subscribe"]>[0] | undefined;
    vi.mocked(client.subscribe).mockImplementation((next) => { listener = next; return () => undefined; });
    const repositories = new GitRepositoryStore(client);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} repositories={repositories} />); await settle(); });
    expect(vi.mocked(client.diff).mock.calls.map((call) => call[3])).toEqual(["YQ=="]);

    // The tab's file changes while its repository does not, so the shared
    // observation is never re-acquired. The live subscription must still follow
    // the tab rather than keep fetching the path it was created with.
    await act(async () => {
      renderer.update(<GitDiffSurface {...props(client)} repositories={repositories} tab={{ ...tab, gitPath: "Yg==" }} />);
      await settle();
    });
    const changed = { ...both, generation: "9", sourceGeneration: "moved" };
    await act(async () => {
      listener?.({ kind: "status", rootToken: "root", watchId: "diff-watch", status: changed });
      await settle(); await settle();
    });
    expect(vi.mocked(client.diff).mock.calls.map((call) => call[3])).toEqual(["YQ==", "Yg=="]);
    await act(async () => { renderer.unmount(); });
  });

  it("presents binary changes safely without constructing a text diff", async () => {
    const client = mockClient();
    vi.mocked(client.diff).mockResolvedValueOnce({ diff: { ...diff, binary: true, oldContent: undefined, newContent: undefined }, status });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} />); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).toContain("Binary changes cannot be displayed");
    expect(renderer.root.findAllByProps({ "data-original": "old\n" })).toHaveLength(0);
    await act(async () => { renderer.unmount(); });
  });

  it("keeps an origin-root observation and reloads after the active pane leaves its repository", async () => {
    const client = mockClient();
    let listener: Parameters<GitWorkspaceClient["subscribe"]>[0] | undefined;
    vi.mocked(client.subscribe).mockImplementation((next) => { listener = next; return () => undefined; });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} activeRoot={{ ...root, path: "/other", token: "other" }} />); await settle(); });
    expect(client.diff).toHaveBeenCalledTimes(1);
    const changed = { ...status, generation: "8", sourceGeneration: "changed" };
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
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: stagedStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue({ diff: { ...diff, target: "staged" }, status: stagedStatus });
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
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: submoduleStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue({ diff, status: submoduleStatus });
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
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: renameStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue({ diff: renameDiff, status: renameStatus });
    const renameTab = { ...tab, gitTarget: "staged" as const, gitOriginalPath: "b2xk" };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitDiffSurface {...props(client)} tab={renameTab} />); await settle(); });
    expect(client.diff).toHaveBeenCalledWith(scope, expect.objectContaining({ token: "root" }), "repo", "YQ==", "b2xk", "staged", expect.any(AbortSignal));
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
    vi.mocked(client.watch).mockImplementation(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status: copyStatus, release: vi.fn() }));
    vi.mocked(client.diff).mockResolvedValue({ diff: { ...diff, originalPath: "b2xk" }, status: copyStatus });
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
const diff: GitDiff = { repository: status.repository, target: "unstaged", path: "YQ==", displayPath: "a", oldContent: new TextEncoder().encode("old\n"), newContent: new TextEncoder().encode("new\n"), sourceGeneration: "diff-source", binary: false, tooLarge: false, oldMissing: false, newMissing: false, hunkCount: 2 };
function mockClient(): GitWorkspaceClient {
  return {
    status: vi.fn(async () => status),
    watch: vi.fn(async (activeScope, activeRoot) => ({ watchId: "diff-watch", rootToken: activeRoot.token, connectionEpoch: activeScope.terminalEpoch, status, release: vi.fn() })),
    diff: vi.fn(async () => ({ diff, status })),
    prepareDiscard: vi.fn(async () => "one-time"),
    mutate: vi.fn(async () => ({ exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied" as const, status })),
    commit: vi.fn(),
    subscribe: vi.fn(() => () => undefined),
  };
}
function props(client: GitWorkspaceClient) {
  return { tab, scope, activeRoot: root, client, repositories: new GitRepositoryStore(client), canWrite: true, onMessage: vi.fn(), onStatus: vi.fn() };
}
async function settle() { await Promise.resolve(); await new Promise((resolve) => setTimeout(resolve, 0)); }

// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitStatusSnapshot, GitWorkspaceClient } from "./types";
import { GitSidebar } from "./GitSidebar";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Per-row stage/discard buttons became one right-click menu in Phase 11, so
 * every row action is reached the way a user reaches it: open the row's menu,
 * then pick the item out of it.
 */
async function rowMenuItem(renderer: ReturnType<typeof create>, displayPath: string, itemId: string) {
  const row = renderer.root.findAll((node) => node.props.className === "git-file"
    && typeof node.props.title === "string" && node.props.title.startsWith(`${displayPath} ·`))[0];
  await act(async () => { row.props.onContextMenu({ preventDefault: vi.fn(), clientX: 10, clientY: 10 }); });
  return renderer.root.findByProps({ "data-menu-item": itemId });
}

describe("GitSidebar", () => {
  it("groups staged, unstaged, untracked, conflict and ignored entries with exact counts", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps()} status={status()} />); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Staged");
    expect(text).toContain("Changes");
    expect(text).toContain("Untracked");
    expect(text).toContain("Merge changes");
    expect(text).toContain("Ignored");
    await act(async () => { renderer.unmount(); });
  });

  it("does not invoke a discard when its confirmation is cancelled", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} status={status()} />); });
    const discard = await rowMenuItem(renderer, "changed.txt", "discard");
    await act(async () => { discard.props.onClick(); });
    expect(renderer.root.findAllByProps({ role: "alertdialog" })).toHaveLength(1);
    const cancel = renderer.root.findAllByType("button").find((button) => button.props.children === "Cancel");
    await act(async () => { cancel?.props.onClick(); await Promise.resolve(); });
    expect(props.client.prepareDiscard).not.toHaveBeenCalled();
    expect(props.client.mutate).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
  });

  it("rejects an empty commit message before invoking Git", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} status={status()} />); });
    const form = renderer.root.findByType("form");
    await act(async () => { form.props.onSubmit({ preventDefault: vi.fn() }); });
    expect(props.client.commit).not.toHaveBeenCalled();
    expect(JSON.stringify(renderer.toJSON())).toContain("Enter a commit message");
    await act(async () => { renderer.unmount(); });
  });

  it("bounds the initial DOM for large repositories while preserving the exact total", async () => {
    const large = status();
    large.entries = Array.from({ length: 2_000 }, (_, index) => entry(`file-${index}.txt`));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps()} status={large} />); });
    expect(renderer.root.findAllByProps({ className: "git-file" })).toHaveLength(200);
    expect(JSON.stringify(renderer.toJSON())).toContain('"Show ","500"," more…"');
    expect(JSON.stringify(renderer.toJSON())).toContain('"2000"');
    await act(async () => { renderer.unmount(); });
  });

  it("surfaces failing hook stdout/stderr and preserves the commit message", async () => {
    const props = baseProps();
    vi.mocked(props.client.commit).mockResolvedValueOnce({ exitCode: 1, stdout: "checking files", stderr: "pre-commit rejected", applied: false, refreshFailed: false, refreshError: "", outcome: "notApplied" });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} status={status()} />); });
    const textarea = renderer.root.findByType("textarea");
    await act(async () => { textarea.props.onChange({ target: { value: "message" } }); });
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); await settle(); });
    expect(props.client.commit).toHaveBeenCalledWith(scope, root, "repo", "1", "message");
    expect(JSON.stringify(renderer.toJSON())).toContain("pre-commit rejected");
    expect(renderer.root.findByType("textarea").props.value).toBe("message");
    await act(async () => { renderer.unmount(); });
  });

  it("cancels a confirmed discard if the connection generation changed while the dialog was open", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} status={status()} />); });
    const discard = await rowMenuItem(renderer, "changed.txt", "discard");
    await act(async () => { discard.props.onClick(); });
    await act(async () => { renderer.update(<GitSidebar {...props} scope={{ ...scope, terminalEpoch: 2 }} status={status()} />); });
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); });
    expect(props.client.prepareDiscard).not.toHaveBeenCalled();
    expect(props.client.mutate).not.toHaveBeenCalled();
    expect(props.onMessage).toHaveBeenCalledWith(expect.stringContaining("connection changed"));
    await act(async () => { renderer.unmount(); });
  });

  it("requires confirmation before discarding a staged file", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} status={status()} />); });
    const discard = await rowMenuItem(renderer, "staged.txt", "discard");
    await act(async () => { discard.props.onClick(); });
    expect(props.client.prepareDiscard).not.toHaveBeenCalled();
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); });
    expect(props.client.prepareDiscard).toHaveBeenCalledWith(scope, root, "repo", expect.objectContaining({ kind: "discardFile", target: "staged" }));
    expect(props.client.mutate).toHaveBeenCalledWith(scope, root, "repo", expect.objectContaining({ kind: "discardFile", target: "staged", confirmationToken: "confirmed" }));
    await act(async () => { renderer.unmount(); });
  });

  it("reports a completed commit separately from a failed status refresh", async () => {
    const props = baseProps();
    vi.mocked(props.client.commit).mockResolvedValueOnce({ exitCode: 0, stdout: "created", stderr: "", applied: true, refreshFailed: true, refreshError: "root changed", outcome: "applied" });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} status={status()} />); });
    await act(async () => { renderer.root.findByType("textarea").props.onChange({ target: { value: "message" } }); });
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); await settle(); });
    expect(renderer.root.findByType("textarea").props.value).toBe("");
    expect(JSON.stringify(renderer.toJSON())).toContain("Commit completed, but status refresh failed: root changed");
    expect(props.onMessage).toHaveBeenCalledWith(expect.stringContaining("Status refresh failed: root changed"));
    await act(async () => { renderer.unmount(); });
  });

  it("presents submodule state but disables unsupported pointer mutations", async () => {
    const value = status();
    value.entries.push(entry("module", { submodule: true, submoduleState: "S.M.", worktreeKind: "modified" }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps()} status={value} />); });
    expect((await rowMenuItem(renderer, "module", "stage")).props.disabled).toBe(true);
    expect((await rowMenuItem(renderer, "module", "discard")).props.disabled).toBe(true);
    expect(JSON.stringify(renderer.toJSON())).toContain("actions unavailable");
    await act(async () => { renderer.unmount(); });
  });

  it("fails closed on a structured oversized status without calling the tree clean", async () => {
    const value = { ...status(), entries: [], authoritative: false, oversized: true, totalEntryCount: "900000", error: "snapshot exceeds the control-frame budget" };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps()} status={value} />); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Repository status is too large");
    expect(text).toContain("900000 entries were detected");
    expect(text).not.toContain("Working tree clean");
    expect(renderer.root.findAllByType("form")).toHaveLength(0);
    await act(async () => { renderer.unmount(); });
  });

  it("discloses bounded copy classification instead of silently relabeling semantics", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps()} status={{ ...status(), copyDetectionIncomplete: true }} />); });
    expect(JSON.stringify(renderer.toJSON())).toContain("some copies may appear as additions");
    await act(async () => { renderer.unmount(); });
  });
});

function baseProps() {
  const client: GitWorkspaceClient = {
    status: vi.fn(), watch: vi.fn(), diff: vi.fn(), prepareDiscard: vi.fn(async () => "confirmed"),
    mutate: vi.fn(async () => ({ exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied" as const, status: status() })), commit: vi.fn(), subscribe: vi.fn(() => () => undefined),
  };
  return { client, scope, root, loading: false, disabled: false, onOpenDiff: vi.fn(), onRefresh: vi.fn(), onStatus: vi.fn(), onMessage: vi.fn() };
}

const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 1, sessionId: "$1", paneId: "%1" };
const root: ActiveRoot = { token: "root", path: "/repo", cwd: "/repo", paneId: "%1", gitWorktree: true, revision: "1" };
function entry(path: string, overrides: Record<string, unknown> = {}) { return { path: btoa(path), displayPath: path, indexKind: "none", worktreeKind: "modified", indexStatus: ".", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false, ...overrides } as GitStatusSnapshot["entries"][number]; }
function status(): GitStatusSnapshot { return { repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "1", sourceGeneration: "source", authoritative: true, entries: [entry("staged.txt", { indexKind: "modified", worktreeKind: "none" }), entry("changed.txt"), entry("new.txt", { untracked: true, worktreeKind: "untracked" }), entry("conflict.txt", { conflicted: true, conflictCode: "UU", indexKind: "unmerged", worktreeKind: "unmerged" }), entry("ignored.log", { ignored: true, worktreeKind: "ignored" })] }; }
async function settle() { await Promise.resolve(); await new Promise((resolve) => setTimeout(resolve, 0)); }

// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitCommandResult, GitStatusSnapshot } from "./types";
import type { WorkspaceGitState } from "./useWorkspaceGit";
import type { GitRepositoryHandle } from "./repositoryStore";
import { rowCommandRegistry } from "../../commands/rowCommands";
import { GitSidebar } from "./GitSidebar";
import { INTERNAL_PATH_DRAG_TYPE } from "../terminal/internalPathDrag";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * Row actions have two ways in — the hover buttons and the right-click menu —
 * and both are reached here the way a user reaches them, through the row.
 */
function gitRow(renderer: ReturnType<typeof create>, displayPath: string) {
  return renderer.root.findAll((node) => node.props.className === "git-file"
    && typeof node.props.title === "string" && node.props.title.startsWith(`${displayPath} ·`))[0];
}

/** The `<li>` a row lives in, which is what carries its hover buttons. */
function gitRowItem(renderer: ReturnType<typeof create>, displayPath: string) {
  let node = gitRow(renderer, displayPath).parent;
  while (node && node.type !== "li") node = node.parent;
  if (!node) throw new Error(`no row for ${displayPath}`);
  return node;
}

function rowActionButton(renderer: ReturnType<typeof create>, displayPath: string, label: string) {
  return gitRowItem(renderer, displayPath).findByProps({ "aria-label": label });
}

async function rowMenuItem(renderer: ReturnType<typeof create>, displayPath: string, itemId: string) {
  const row = gitRow(renderer, displayPath);
  await act(async () => { row.props.onContextMenu({ preventDefault: vi.fn(), clientX: 10, clientY: 10 }); });
  return renderer.root.findByProps({ "data-menu-item": itemId });
}

describe("GitSidebar", () => {
  it("writes a private canonical same-host payload for Git rows", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps()} />); });
    const values = new Map<string, string>();
    const dataTransfer = { effectAllowed: "all", setData: (type: string, value: string) => values.set(type, value) };
    gitRow(renderer, "changed.txt").props.onDragStart({ dataTransfer });
    expect(JSON.parse(values.get(INTERNAL_PATH_DRAG_TYPE)!)).toMatchObject({ version: 1, hostProfileId: "local", serverIdentity: "s", path: "/repo/changed.txt" });
    expect(JSON.parse(values.get(INTERNAL_PATH_DRAG_TYPE)!).gestureId).toEqual(expect.any(String));
    expect(gitRow(renderer, "changed.txt").props.draggable).toBe(true);
    expect(gitRow(renderer, "changed.txt").props.onDragEnd).toBeTypeOf("function");
    await act(async () => { renderer.unmount(); });
  });

  it("fails closed instead of dragging a lossy Git display path", async () => {
    const value = status();
    // A byte path that is not UTF-8: the display path is a lossy rendering of
    // it, so no absolute path can be derived and nothing may be dragged.
    value.entries = [entry("lossy-name", { path: btoa("raw-\xff") })];
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    expect(gitRow(renderer, "lossy-name").props.draggable).toBe(false);
    await act(async () => { renderer.unmount(); });
  });

  it("groups staged, unstaged, untracked and conflict entries, and shows nothing ignored", async () => {
    const value = status();
    value.entries.push(entry("image.png", { binary: true }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Staged");
    expect(text).toContain("Changes");
    expect(text).toContain("Untracked");
    expect(text).toContain("Merge changes");
    // An ignored file is not a change, so the panel does not carry a section
    // for it, and "binary" is not a status worth a second line under the row.
    expect(text).not.toContain("Ignored");
    expect(text).not.toContain("ignored.log");
    expect(text).not.toContain("binary");
    await act(async () => { renderer.unmount(); });
  });

  it("renders a row as file icon, name, dimmed directory and a trailing status letter", async () => {
    const value = { ...status(), entries: [entry("src/app/main.rs")] };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    const row = gitRow(renderer, "src/app/main.rs");
    expect(row.findByProps({ className: "git-file-icon" }).props.style).toEqual({ color: "var(--term-1)" });
    expect(row.findByProps({ className: "git-file-name" }).children).toEqual(["main.rs"]);
    expect(row.findByProps({ className: "git-file-dir" }).children).toEqual(["src/app"]);
    // The status letter is last, so CSS can push it to the right edge and the
    // hover buttons can take exactly its place.
    const last = row.children[row.children.length - 1];
    expect(typeof last === "string" ? last : last.props.className).toBe("git-state modified");
    expect(typeof last === "string" ? last : last.children).toEqual(["M"]);
    await act(async () => { renderer.unmount(); });
  });

  it("stages and unstages from the row's hover buttons", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { rowActionButton(renderer, "changed.txt", "Stage file").props.onClick(); await settle(); });
    expect(props.git.handle!.mutate).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "stageFile", target: "unstaged" }));

    // The staged copy of a path offers the opposite direction, in the same spot.
    await act(async () => { rowActionButton(renderer, "staged.txt", "Unstage file").props.onClick(); await settle(); });
    expect(props.git.handle!.mutate).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "unstageFile", target: "staged" }));
    await act(async () => { renderer.unmount(); });
  });

  it("routes the row's discard button through the same confirmation the menu uses", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { rowActionButton(renderer, "changed.txt", "Discard changes").props.onClick(); });
    expect(renderer.root.findAllByProps({ role: "alertdialog" })).toHaveLength(1);
    expect(props.git.handle!.prepareDiscard).not.toHaveBeenCalled();
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); });
    expect(props.git.handle!.prepareDiscard).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "discardFile", target: "unstaged" }));
    expect(props.git.handle!.mutate).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "discardFile", confirmationToken: "confirmed" }));
    await act(async () => { renderer.unmount(); });
  });

  it("offers no hover buttons where the mutation does not exist", async () => {
    const value = status();
    value.entries.push(entry("module", { submodule: true, submoduleState: "S.M.", worktreeKind: "modified" }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    // An untracked file is deleted rather than reverted, so its button says so.
    expect(rowActionButton(renderer, "new.txt", "Delete untracked file")).toBeTruthy();
    expect(gitRowItem(renderer, "module").findAllByProps({ "aria-label": "Stage file" })).toHaveLength(0);
    expect(gitRowItem(renderer, "conflict.txt").findAllByProps({ className: "git-row-actions" })).toHaveLength(0);

    // A resynchronizing status disables every mutation, hover buttons included.
    await act(async () => { renderer.update(<GitSidebar {...baseProps(gitState({ status: { ...value, authoritative: false } }))} />); });
    expect(renderer.root.findAllByProps({ className: "git-row-actions" })).toHaveLength(0);
    await act(async () => { renderer.unmount(); });
  });

  it("re-reads the repository from the panel header", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "Refresh Git status" }).props.onClick(); });
    expect(props.git.handle!.refresh).toHaveBeenCalledTimes(1);
    await act(async () => { renderer.unmount(); });
  });

  it("does not invoke a discard when its confirmation is cancelled", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const discard = await rowMenuItem(renderer, "changed.txt", "discard");
    await act(async () => { discard.props.onClick(); });
    expect(renderer.root.findAllByProps({ role: "alertdialog" })).toHaveLength(1);
    const cancel = renderer.root.findAllByType("button").find((button) => button.props.children === "Cancel");
    await act(async () => { cancel?.props.onClick(); await Promise.resolve(); });
    expect(props.git.handle!.prepareDiscard).not.toHaveBeenCalled();
    expect(props.git.handle!.mutate).not.toHaveBeenCalled();
    await act(async () => { renderer.unmount(); });
  });

  it("rejects an empty commit message before invoking Git", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const form = renderer.root.findByType("form");
    await act(async () => { form.props.onSubmit({ preventDefault: vi.fn() }); });
    expect(props.git.handle!.commit).not.toHaveBeenCalled();
    expect(JSON.stringify(renderer.toJSON())).toContain("Enter a commit message");
    await act(async () => { renderer.unmount(); });
  });

  it("bounds the initial DOM for large repositories while preserving the exact total", async () => {
    const large = status();
    large.entries = Array.from({ length: 2_000 }, (_, index) => entry(`file-${index}.txt`));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: large }))} />); });
    expect(renderer.root.findAllByProps({ className: "git-file" })).toHaveLength(200);
    expect(JSON.stringify(renderer.toJSON())).toContain('"Show ","500"," more…"');
    expect(JSON.stringify(renderer.toJSON())).toContain('"2000"');
    await act(async () => { renderer.unmount(); });
  });

  it("surfaces failing hook stdout/stderr and preserves the commit message", async () => {
    const props = baseProps();
    vi.mocked(props.git.handle!.commit).mockResolvedValueOnce({ exitCode: 1, stdout: "checking files", stderr: "pre-commit rejected", applied: false, refreshFailed: false, refreshError: "", outcome: "notApplied" });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const textarea = renderer.root.findByType("textarea");
    await act(async () => { textarea.props.onChange({ target: { value: "message" } }); });
    await act(async () => { renderer.root.findByType("form").props.onSubmit({ preventDefault: vi.fn() }); await settle(); });
    expect(props.git.handle!.commit).toHaveBeenCalledWith("repo", "1", "message");
    expect(JSON.stringify(renderer.toJSON())).toContain("pre-commit rejected");
    expect(renderer.root.findByType("textarea").props.value).toBe("message");
    await act(async () => { renderer.unmount(); });
  });

  it("cancels a confirmed discard if the connection generation changed while the dialog was open", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const discard = await rowMenuItem(renderer, "changed.txt", "discard");
    await act(async () => { discard.props.onClick(); });
    await act(async () => { renderer.update(<GitSidebar {...props} scope={{ ...scope, terminalEpoch: 2 }} />); });
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); });
    expect(props.git.handle!.prepareDiscard).not.toHaveBeenCalled();
    expect(props.git.handle!.mutate).not.toHaveBeenCalled();
    expect(props.onMessage).toHaveBeenCalledWith(expect.stringContaining("connection changed"));
    await act(async () => { renderer.unmount(); });
  });

  it("requires confirmation before discarding a staged file", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const discard = await rowMenuItem(renderer, "staged.txt", "discard");
    await act(async () => { discard.props.onClick(); });
    expect(props.git.handle!.prepareDiscard).not.toHaveBeenCalled();
    const confirm = renderer.root.findAllByType("button").find((button) => button.props.children === "Discard");
    await act(async () => { confirm?.props.onClick(); await settle(); });
    expect(props.git.handle!.prepareDiscard).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "discardFile", target: "staged" }));
    expect(props.git.handle!.mutate).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "discardFile", target: "staged", confirmationToken: "confirmed" }));
    await act(async () => { renderer.unmount(); });
  });

  it("reports a completed commit separately from a failed status refresh", async () => {
    const props = baseProps();
    vi.mocked(props.git.handle!.commit).mockResolvedValueOnce({ exitCode: 0, stdout: "created", stderr: "", applied: true, refreshFailed: true, refreshError: "root changed", outcome: "applied" });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
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
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    expect((await rowMenuItem(renderer, "module", "stage")).props.disabled).toBe(true);
    expect((await rowMenuItem(renderer, "module", "discard")).props.disabled).toBe(true);
    expect(JSON.stringify(renderer.toJSON())).toContain("actions unavailable");
    await act(async () => { renderer.unmount(); });
  });

  it("fails closed on a structured oversized status without calling the tree clean", async () => {
    const value = { ...status(), entries: [], authoritative: false, oversized: true, totalEntryCount: "900000", error: "snapshot exceeds the control-frame budget" };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Repository status is too large");
    expect(text).toContain("900000 entries were detected");
    expect(text).not.toContain("Working tree clean");
    expect(renderer.root.findAllByType("form")).toHaveLength(0);
    await act(async () => { renderer.unmount(); });
  });

  it("reaches stage, unstage and discard from the command registry, on the last row focused", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    // Nothing focused yet: the palette does not guess which change is meant.
    expect(rowCommandRegistry.available()).toEqual([]);

    await act(async () => { gitRow(renderer, "changed.txt").props.onFocus(); });
    expect(rowCommandRegistry.available()).toEqual(["git.openDiff", "git.stage", "git.discard"]);
    await act(async () => { rowCommandRegistry.run("git.stage"); await settle(); });
    expect(props.git.handle!.mutate).toHaveBeenCalledWith("repo", expect.objectContaining({ kind: "stageFile", target: "unstaged" }));

    // The staged copy of a path offers the opposite direction.
    await act(async () => { gitRow(renderer, "staged.txt").props.onFocus(); });
    expect(rowCommandRegistry.available()).toEqual(["git.openDiff", "git.unstage", "git.discard"]);
    // Discard still goes through its own confirmation — the palette is another
    // way in, not a way around the guard.
    await act(async () => { rowCommandRegistry.run("git.discard"); });
    expect(renderer.root.findAllByProps({ role: "alertdialog" })).toHaveLength(1);

    await act(async () => { renderer.unmount(); });
    expect(rowCommandRegistry.available()).toEqual([]);
  });

  it("offers no row mutations for a submodule or while the status is resynchronizing", async () => {
    const value = status();
    value.entries.push(entry("module", { submodule: true, submoduleState: "S.M.", worktreeKind: "modified" }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: value }))} />); });
    await act(async () => { gitRow(renderer, "module").props.onFocus(); });
    expect(rowCommandRegistry.available()).toEqual(["git.openDiff"]);

    await act(async () => { renderer.update(<GitSidebar {...baseProps(gitState({ status: { ...value, authoritative: false } }))} />); });
    await act(async () => { gitRow(renderer, "changed.txt").props.onFocus(); });
    expect(rowCommandRegistry.available()).toEqual(["git.openDiff"]);
    await act(async () => { renderer.unmount(); });
  });

  it("summarizes a Git rejection and keeps the raw output behind a disclosure", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ error: "git_rejected: fatal: pathspec did not match" }))} />); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Git refused that change.");
    expect(text).toContain("Details");
    expect(text).toContain("pathspec did not match");
    await act(async () => { renderer.unmount(); });
  });

  it("discloses bounded copy classification instead of silently relabeling semantics", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: { ...status(), copyDetectionIncomplete: true } }))} />); });
    expect(JSON.stringify(renderer.toJSON())).toContain("some copies may appear as additions");
    await act(async () => { renderer.unmount(); });
  });

  it("shows what is pending on the exact row a mutation targets", async () => {
    let settleMutation!: () => void;
    const props = baseProps(gitState({
      mutate: vi.fn(() => new Promise<GitCommandResult>((resolve) => {
        settleMutation = () => resolve({ exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied" });
      })),
    }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const stage = await rowMenuItem(renderer, "changed.txt", "stage");
    await act(async () => { stage.props.onClick(); await Promise.resolve(); });
    // The target is visible while the host is still the authority on whether
    // it happened.
    expect(JSON.stringify(renderer.toJSON())).toContain("staging…");
    expect(gitRow(renderer, "changed.txt").props["aria-busy"]).toBe(true);
    await act(async () => { settleMutation(); await settle(); });
    expect(JSON.stringify(renderer.toJSON())).not.toContain("staging…");
    await act(async () => { renderer.unmount(); });
  });

  it("does not rebuild status rows while the commit message is typed", async () => {
    const wide = { ...status(), entries: Array.from({ length: 400 }, (_, index) => entry(`file-${index}.txt`, { indexKind: "modified", worktreeKind: "modified" })) };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: wide }))} />); });
    const before = rowHandlers(renderer);
    expect(before.length).toBeGreaterThan(100);

    const message = renderer.root.findByProps({ "aria-label": "Commit message" });
    await act(async () => { message.props.onChange({ target: { value: "w" } }); });
    await act(async () => { message.props.onChange({ target: { value: "wo" } }); });
    const after = rowHandlers(renderer);
    expect(after).toHaveLength(before.length);
    // Identical handler identities prove the memoized groups were never
    // re-rendered, so a keystroke costs nothing per row.
    for (const [index, handler] of after.entries()) expect(handler).toBe(before[index]);
    expect(renderer.root.findByProps({ "aria-label": "Commit message" }).props.value).toBe("wo");
    await act(async () => { renderer.unmount(); });
  });

  it("keeps the commit form mounted with nothing staged, so Push survives the commit that emptied it", async () => {
    const clean = { ...status(), entries: [] };
    const props = baseProps(gitState({ status: clean }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    const commitButton = renderer.root.findAllByType("button").find((button) => Array.isArray(button.props.children) && button.props.children[0] === "Commit ");
    expect(commitButton?.props.disabled).toBe(true);
    const pushButton = renderer.root.findByProps({ "aria-label": "Push to upstream" });
    expect(pushButton.props.disabled).toBe(false);
    await act(async () => { pushButton.props.onClick(); await settle(); });
    expect(props.git.handle!.push).toHaveBeenCalledWith("repo", "1");
    expect(props.git.handle!.commit).not.toHaveBeenCalled();
    expect(JSON.stringify(renderer.toJSON())).toContain("Pushed to origin/main.");
    await act(async () => { renderer.unmount(); });
  });

  it("offers no Push in a repository that has no commit to publish", async () => {
    const initial = { ...status(), repository: { ...status().repository, initial: true } };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: initial }))} />); });
    expect(renderer.root.findByProps({ "aria-label": "Push to upstream" }).props.disabled).toBe(true);
    await act(async () => { renderer.unmount(); });
  });

  it("commits and then pushes from the split button's menu", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { renderer.root.findByType("textarea").props.onChange({ target: { value: "message" } }); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "More commit actions" }).props.onClick({ currentTarget: anchorTarget() }); });
    const item = renderer.root.findByProps({ "data-menu-item": "commitAndPush" });
    await act(async () => { item.props.onClick(); await settle(); });
    expect(props.git.handle!.commit).toHaveBeenCalledWith("repo", "1", "message");
    expect(props.git.handle!.push).toHaveBeenCalledWith("repo", "1");
    // Both halves are reported, not just the last one.
    expect(JSON.stringify(renderer.toJSON())).toContain("Committed. Pushed to origin/main.");
    await act(async () => { renderer.unmount(); });
  });

  it("does not talk to the remote when the commit it was chained to did not happen", async () => {
    const props = baseProps();
    vi.mocked(props.git.handle!.commit).mockResolvedValueOnce({ exitCode: 1, stdout: "", stderr: "pre-commit rejected", applied: false, refreshFailed: false, refreshError: "", outcome: "notApplied" });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { renderer.root.findByType("textarea").props.onChange({ target: { value: "message" } }); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "More commit actions" }).props.onClick({ currentTarget: anchorTarget() }); });
    await act(async () => { renderer.root.findByProps({ "data-menu-item": "commitAndPush" }).props.onClick(); await settle(); });
    expect(props.git.handle!.commit).toHaveBeenCalledTimes(1);
    expect(props.git.handle!.push).not.toHaveBeenCalled();
    const failed = JSON.stringify(renderer.toJSON());
    expect(failed).toContain("pre-commit rejected");
    expect(failed).not.toContain("Pushing…");
    await act(async () => { renderer.unmount(); });
  });

  it("keeps a half-typed commit message through a transient resynchronization", async () => {
    const props = baseProps();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { renderer.root.findByType("textarea").props.onChange({ target: { value: "half typed" } }); });
    // The banner the panel already shows for this state; the draft is not a
    // casualty of it.
    await act(async () => { renderer.update(<GitSidebar {...baseProps(gitState({ status: { ...status(), authoritative: false } }))} />); });
    expect(renderer.root.findByType("textarea").props.value).toBe("half typed");
    expect(renderer.root.findByProps({ "aria-label": "Push to upstream" }).props.disabled).toBe(true);
    await act(async () => { renderer.unmount(); });
  });

  it("does not leave a progress line claiming a push is still running after it failed", async () => {
    const props = baseProps();
    vi.mocked(props.git.handle!.push).mockRejectedValueOnce(new Error("git_push_rejected: ! [rejected] master -> master (non-fast-forward)"));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "Push to upstream" }).props.onClick(); await settle(); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).not.toContain("Pushing…");
    expect(text).toContain("The remote rejected the push (non-fast-forward?). Pull or rebase first.");
    await act(async () => { renderer.unmount(); });
  });

  it("never reports a push the host could not confirm as one that worked", async () => {
    const props = baseProps();
    vi.mocked(props.git.handle!.push).mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "", applied: false, refreshFailed: false, refreshError: "", outcome: "partialOrUnknown", pushTarget: "origin/main" });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...props} />); });
    await act(async () => { renderer.root.findByProps({ "aria-label": "Push to upstream" }).props.onClick(); await settle(); });
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Push outcome is unknown; check the remote before retrying.");
    expect(text).not.toContain("Pushed to origin/main.");
    await act(async () => { renderer.unmount(); });
  });

  it("treats re-focusing the same row as no change at all", async () => {
    const wide = { ...status(), entries: Array.from({ length: 200 }, (_, index) => entry(`file-${index}.txt`)) };
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<GitSidebar {...baseProps(gitState({ status: wide }))} />); });
    const row = gitRow(renderer, "file-3.txt");
    await act(async () => { row.props.onFocus(); });
    const focused = rowHandlers(renderer);
    await act(async () => { row.props.onFocus(); });
    await act(async () => { row.props.onPointerDown(); });
    const after = rowHandlers(renderer);
    for (const [index, handler] of after.entries()) expect(handler).toBe(focused[index]);
    await act(async () => { renderer.unmount(); });
  });

});

/** What `anchorForElement` needs from the button a menu was opened from. */
function anchorTarget() {
  return { getBoundingClientRect: () => ({ left: 10, bottom: 20 }) };
}

/** Every row's click handler, whose identity changes if its group re-renders. */
function rowHandlers(renderer: ReturnType<typeof create>) {
  return renderer.root
    .findAll((node) => node.props.className === "git-file")
    .map((node) => node.props.onClick as () => void);
}

/**
 * The sidebar renders one shared repository observation and mutates through it,
 * so the test double is that observation rather than a protocol client.
 */
function gitState(overrides: Partial<WorkspaceGitState & GitRepositoryHandle> = {}): WorkspaceGitState {
  const { status: snapshot, loading, error, ...handle } = overrides;
  return {
    status: "status" in overrides ? snapshot : status(),
    loading: loading ?? false,
    ...(error !== undefined ? { error } : {}),
    handle: {
      state: vi.fn(() => ({ loading: false })),
      subscribe: vi.fn(() => () => undefined),
      refresh: vi.fn(async () => undefined),
      diff: vi.fn(),
      mutate: vi.fn(async () => applied()),
      prepareDiscard: vi.fn(async () => "confirmed"),
      commit: vi.fn(async () => applied()),
      push: vi.fn(async () => pushed()),
      ...handle,
    },
  };
}

function applied(): GitCommandResult {
  return { exitCode: 0, stdout: "", stderr: "", applied: true, refreshFailed: false, refreshError: "", outcome: "applied", status: status() };
}

function pushed(): GitCommandResult {
  return { ...applied(), pushTarget: "origin/main" };
}

function baseProps(git: WorkspaceGitState = gitState()) {
  return { git, scope, root, disabled: false, onOpenDiff: vi.fn(), onMessage: vi.fn() };
}

const scope: FileWorkspaceScope = { clientId: "c", hostProfileId: "local", serverIdentity: "s", generation: 1, terminalEpoch: 1, sessionId: "$1", paneId: "%1" };
const root: ActiveRoot = { token: "root", path: "/repo", cwd: "/repo", paneId: "%1", gitWorktree: true, revision: "1" };
function entry(path: string, overrides: Record<string, unknown> = {}) { return { path: btoa(path), displayPath: path, indexKind: "none", worktreeKind: "modified", indexStatus: ".", worktreeStatus: "M", conflicted: false, untracked: false, ignored: false, submodule: false, symlink: false, binary: false, ...overrides } as GitStatusSnapshot["entries"][number]; }
function status(): GitStatusSnapshot { return { repository: { id: "repo", worktreeRoot: "/repo", initial: false, detachedHead: false, headName: "main" }, generation: "1", sourceGeneration: "source", authoritative: true, entries: [entry("staged.txt", { indexKind: "modified", worktreeKind: "none" }), entry("changed.txt"), entry("new.txt", { untracked: true, worktreeKind: "untracked" }), entry("conflict.txt", { conflicted: true, conflictCode: "UU", indexKind: "unmerged", worktreeKind: "unmerged" }), entry("ignored.log", { ignored: true, worktreeKind: "ignored" })] }; }
async function settle() { await Promise.resolve(); await new Promise((resolve) => setTimeout(resolve, 0)); }

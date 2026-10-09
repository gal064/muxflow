// @vitest-environment jsdom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";
import { quickOpenItems, quickOpenScore } from "./quickOpen";
import type { MergedWorkspaceRow } from "./mergedWorkspaceRows";
import type { ActiveRoot, FileSearchResults, FileWorkspaceScope } from "../files/types";
import type { SelectableTab } from "../shell/model";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const scope: FileWorkspaceScope = { clientId: "client", hostProfileId: "local", serverIdentity: "server", generation: 1, terminalEpoch: 1, sessionId: "$1", paneId: "%1" };
const activeRoot: ActiveRoot = { path: "/repo", cwd: "/repo", token: "root", paneId: "%1", gitWorktree: true, revision: "1" };
const workspace: MergedWorkspaceRow = {
  key: "local\0$1", hostProfileId: "local", letter: "L", phase: "connected", canMutate: true,
  scope: { hostProfileId: "local", connectionKey: "local", connectionEpoch: 1, serverIdentity: "server", generation: 1 },
  session: { id: "$1", name: "work", windowCount: 1, attachedClients: 0 }, path: "/repo", active: true,
  attention: "none", unread: 0, pinned: false, working: false, agents: [], agentOverflow: 0,
};
const tab: SelectableTab = { kind: "app", key: "app:editor", id: "editor", title: "work.ts", appKind: "file", resource: "/repo/work.ts", order: 0, preview: false, canMoveLeft: false, canMoveRight: false };
const files = [{ path: "/repo/work.ts", relativePath: "work.ts", score: 1040 }, { path: "/repo/workspace.ts", relativePath: "workspace.ts", score: 1035 }];
const hostLabel = () => "Local";

let container: HTMLDivElement;
let root: Root;
let props: ComponentProps<typeof WorkspaceSwitcher>;
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((answer) => { resolve = answer; }); return { resolve, promise }; }
async function render() { await act(async () => root.render(<WorkspaceSwitcher {...props} />)); }
async function type(query: string) {
  await act(async () => {
    const input = container.querySelector('input')!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, query);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function key(value: string) { await act(async () => container.querySelector('input')!.dispatchEvent(new KeyboardEvent("keydown", { key: value, bubbles: true }))); }

beforeEach(() => {
  vi.useFakeTimers();
  Element.prototype.scrollIntoView = vi.fn();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  props = {
    rows: [workspace], tabs: [tab], client: { resolveActiveRoot: vi.fn(async () => activeRoot), searchFiles: vi.fn(async () => ({ matches: files, complete: true })) },
    scope, pane: { sessionId: "$1", windowId: "@1", cwd: "/repo" },
    hostLabel: "Local", hostLabelFor: hostLabel, stateGlyphs: false,
    onClose: vi.fn(), onSelect: vi.fn(), onSelectTab: vi.fn(), onOpenFile: vi.fn(),
  };
});
afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); });

describe("Quick Open", () => {
  it("focuses the search field and restores prior focus after Escape", async () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    trigger.focus();
    await render();
    expect(document.activeElement).toBe(container.querySelector("input"));
    await key("Escape");
    expect(props.onClose).toHaveBeenCalledOnce();
    await act(async () => root.unmount());
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
    root = createRoot(container);
  });

  it("deduplicates file tabs and ranks names above path-only matches", () => {
    const rows = quickOpenItems([workspace], [tab], files, "work", tab.key, hostLabel);
    expect(rows.map((item) => item.kind)).toEqual(["tab", "workspace", "file"]);
    expect(rows.filter((item) => item.title === "work.ts")).toHaveLength(1);
    expect(quickOpenScore("work.ts", "deep/path", "work")).toBeGreaterThan(quickOpenScore("other.ts", "/work/path", "work")!);
    expect(quickOpenItems([workspace], [tab], files, "", tab.key, hostLabel).map((item) => item.kind)).toEqual(["tab", "workspace"]);
  });

  it("keeps files idle on opening and debounces requests while typing", async () => {
    await render();
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(props.client.resolveActiveRoot).not.toHaveBeenCalled();
    await type("w");
    await act(async () => vi.advanceTimersByTimeAsync(50));
    await type("work");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(props.client.searchFiles).toHaveBeenCalledTimes(1);
    expect(props.client.searchFiles).toHaveBeenCalledWith(scope, activeRoot, props.pane, "work", expect.any(AbortSignal));
    await type("workspace");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(props.client.resolveActiveRoot).toHaveBeenCalledTimes(1);
  });

  it("preserves the selected tab when higher-scoring files arrive", async () => {
    const pending = deferred<FileSearchResults>();
    props.client.searchFiles = vi.fn(() => pending.promise);
    await render(); await type("work"); await key("ArrowDown"); await key("ArrowUp");
    const selected = container.querySelector('[aria-selected=true]')!.id;
    await act(async () => vi.advanceTimersByTimeAsync(100));
    await act(async () => pending.resolve({ matches: Array.from({ length: 75 }, (_, index) => ({ path: `/repo/work-${index}`, relativePath: `work-${index}`, score: 9999 })), complete: true }));
    expect(container.querySelectorAll('[role=option]')).toHaveLength(75);
    expect(container.querySelector('[aria-selected=true]')!.id).toBe(selected);
    await key("Enter");
    expect(props.onSelectTab).toHaveBeenCalledWith(tab);
    expect(props.onOpenFile).not.toHaveBeenCalled();
  });

  it("cancels old queries, drops stale answers, and cancels on closing", async () => {
    const pending = deferred<FileSearchResults>();
    props.client.searchFiles = vi.fn(() => pending.promise);
    await render(); await type("work");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    const signal = vi.mocked(props.client.searchFiles).mock.calls[0][4]!;
    await type("other");
    expect(signal.aborted).toBe(true);
    await act(async () => pending.resolve({ matches: files, complete: true }));
    expect(container.textContent).not.toContain("workspace.ts");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    const lastSignal = vi.mocked(props.client.searchFiles).mock.calls.at(-1)![4]!;
    await act(async () => root.unmount());
    expect(lastSignal.aborted).toBe(true);
    root = createRoot(container);
  });

  it("rebinds search to CWD and connection changes", async () => {
    const pending = deferred<FileSearchResults>();
    props.client.searchFiles = vi.fn(() => pending.promise);
    await render(); await type("work");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    const signal = vi.mocked(props.client.searchFiles).mock.calls[0][4]!;
    props = { ...props, pane: { ...props.pane!, cwd: "/other" }, scope: { ...scope, clientId: "new-client", terminalEpoch: 2 } };
    await render();
    expect(signal.aborted).toBe(true);
    await act(async () => pending.resolve({ matches: files, complete: true }));
    expect(container.textContent).not.toContain("workspace.ts");
    expect(container.textContent).toContain("/other");
  });

  it("keeps navigation usable after file errors and clears empty selection", async () => {
    props.client.searchFiles = vi.fn(async () => { throw new Error("Disconnected"); });
    await render(); await type("work");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(container.textContent).toContain("File search unavailable");
    await key("ArrowDown");
    await key("Enter");
    expect(props.onSelect).toHaveBeenCalledWith(workspace);
    await type("unmatched");
    expect(container.querySelector('input')!.hasAttribute("aria-activedescendant")).toBe(false);
    await key("ArrowDown"); await key("Enter");
    expect(props.onSelect).toHaveBeenCalledTimes(1);
  });
});

// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { Session, Window as TmuxWindow } from "./types";
import type { AppOwnedTab } from "../features/shell/types";
import { useFocusHistoryNavigation, type FocusHistoryNavigationOptions } from "./useFocusHistoryNavigation";

const sessions: Session[] = [
  { id: "$1", name: "one", windowCount: 2, attachedClients: 1, order: 0 },
  { id: "$2", name: "two", windowCount: 1, attachedClients: 1, order: 1 },
];
const windows: TmuxWindow[] = [
  { id: "@1", sessionId: "$1", index: 0, name: "zsh", active: true, layout: "" },
  { id: "@2", sessionId: "$1", index: 1, name: "vim", active: false, layout: "" },
  { id: "@3", sessionId: "$2", index: 0, name: "zsh", active: true, layout: "" },
];
const tab = (id: string, sessionId = "$1", kind: AppOwnedTab["kind"] = "file"): AppOwnedTab => ({
  id, hostProfileId: "local", serverIdentity: "server-a", sessionId, sessionName: "one",
  kind, resource: `/repo/${id}`, title: id, order: 0,
});

type Location = Pick<FocusHistoryNavigationOptions, "activeSessionId" | "activeWindowId" | "selectedAppTabId" | "appTabs">;

/**
 * The hook is driven the way the shell drives it: navigation callbacks are
 * spies, and "the app landed there" is a rerender with the new location —
 * the same two-step the real navigation takes.
 */
function mount(initial: Location) {
  const selectSession = vi.fn();
  const selectWindow = vi.fn();
  const selectAppTab = vi.fn();
  const revealTerminal = vi.fn();
  const setStatus = vi.fn();
  let latest!: ReturnType<typeof useFocusHistoryNavigation>;
  function Harness(props: { location: Location }) {
    latest = useFocusHistoryNavigation({
      ...props.location, revealTerminal, selectAppTab, selectSession, selectWindow, sessions, setStatus, windows,
    });
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  act(() => { renderer = create(<Harness location={initial} />); });
  return {
    get hook() { return latest; },
    landAt: (location: Location) => act(() => { renderer.update(<Harness location={location} />); }),
    revealTerminal, selectAppTab, selectSession, selectWindow, setStatus,
    unmount: () => act(() => renderer.unmount()),
  };
}

const terminal = (windowId: string, appTabs: AppOwnedTab[] = [], sessionId = "$1"): Location =>
  ({ activeSessionId: sessionId, activeWindowId: windowId, appTabs });
const document = (appTabId: string, windowId: string, appTabs: AppOwnedTab[], sessionId = "$1"): Location =>
  ({ activeSessionId: sessionId, activeWindowId: windowId, selectedAppTabId: appTabId, appTabs });

describe("focus history navigation", () => {
  it("goes back from a file to the terminal it was opened from, without recording the step", () => {
    const notes = tab("notes");
    const harness = mount(terminal("@1"));
    harness.landAt(document("notes", "@1", [notes]));
    expect(harness.hook.history.entries).toEqual([
      { sessionId: "$1", windowId: "@1", appTabId: undefined },
      { sessionId: "$1", windowId: "@1", appTabId: "notes" },
    ]);
    expect(harness.hook.canGoBack).toBe(true);
    expect(harness.hook.canGoForward).toBe(false);

    act(() => harness.hook.step("back"));
    // Same window: the terminal is under the document, so it is uncovered rather than switched to.
    expect(harness.revealTerminal).toHaveBeenCalledExactlyOnceWith("$1", "@1");
    expect(harness.selectWindow).not.toHaveBeenCalled();
    harness.landAt(terminal("@1", [notes]));
    expect(harness.hook.history.entries).toHaveLength(2);
    expect(harness.hook.history.cursor).toBe(0);
    expect(harness.hook.canGoBack).toBe(false);
    expect(harness.hook.canGoForward).toBe(true);
    harness.unmount();
  });

  it("returns to the source terminal when the document on screen is closed", () => {
    const notes = tab("notes");
    const harness = mount(terminal("@1"));
    harness.landAt(document("notes", "@1", [notes]));
    // The user switched windows under the document and came back to it: the
    // closest earlier entry is still the terminal, and it is what Close returns to.
    harness.landAt(terminal("@2", [notes]));
    harness.landAt(document("notes", "@2", [notes]));
    expect(harness.hook.history.entries).toHaveLength(4);

    let navigated = false;
    act(() => { navigated = harness.hook.navigateBackFromClosing("notes"); });
    expect(navigated).toBe(true);
    expect(harness.revealTerminal).toHaveBeenCalledExactlyOnceWith("$1", "@2");
    harness.landAt(terminal("@2", []));
    // The closed document is pruned, the cursor sits on the terminal, and nothing was appended.
    expect(harness.hook.history.entries).toEqual([
      { sessionId: "$1", windowId: "@1", appTabId: undefined },
      { sessionId: "$1", windowId: "@2", appTabId: undefined },
    ]);
    expect(harness.hook.history.cursor).toBe(1);
    expect(harness.hook.canGoForward).toBe(false);
    harness.unmount();
  });

  it("reports nowhere to go when the only earlier entry is the closing tab itself", () => {
    const notes = tab("notes");
    const harness = mount(document("notes", "@1", [notes]));
    let navigated = true;
    act(() => { navigated = harness.hook.navigateBackFromClosing("notes"); });
    expect(navigated).toBe(false);
    expect(harness.revealTerminal).not.toHaveBeenCalled();
    expect(harness.selectWindow).not.toHaveBeenCalled();
    harness.unmount();
  });

  it("walks back and forward across terminals and documents in visit order", () => {
    const diff = tab("diff", "$1", "gitDiff");
    const harness = mount(terminal("@1"));
    harness.landAt(document("diff", "@1", [diff]));
    harness.landAt(terminal("@2", [diff]));

    act(() => harness.hook.step("back"));
    expect(harness.selectAppTab).toHaveBeenCalledExactlyOnceWith("$1", "@1", "diff");
    harness.landAt(document("diff", "@1", [diff]));
    act(() => harness.hook.step("back"));
    expect(harness.revealTerminal).toHaveBeenCalledExactlyOnceWith("$1", "@1");
    harness.landAt(terminal("@1", [diff]));
    expect(harness.hook.canGoBack).toBe(false);
    act(() => harness.hook.step("back"));
    expect(harness.setStatus).toHaveBeenCalledWith("Nothing earlier to go back to.");

    act(() => harness.hook.step("forward"));
    expect(harness.selectAppTab).toHaveBeenLastCalledWith("$1", "@1", "diff");
    harness.landAt(document("diff", "@1", [diff]));
    act(() => harness.hook.step("forward"));
    expect(harness.selectWindow).toHaveBeenCalledExactlyOnceWith("@2");
    harness.landAt(terminal("@2", [diff]));
    expect(harness.hook.canGoForward).toBe(false);
    act(() => harness.hook.step("forward"));
    expect(harness.setStatus).toHaveBeenLastCalledWith("Nothing later to go forward to.");
    // Three visits, three entries: the traversals appended nothing.
    expect(harness.hook.history.entries).toHaveLength(3);
    expect(harness.hook.history.cursor).toBe(2);
    harness.unmount();
  });

  it("skips a destination that has been closed and disables the arrow when nothing else is left", () => {
    const notes = tab("notes");
    const harness = mount(terminal("@1"));
    harness.landAt(document("notes", "@1", [notes]));
    harness.landAt(terminal("@2", [notes]));
    expect(harness.hook.canGoBack).toBe(true);
    // Closed from elsewhere — a bulk close, say — while the terminal is showing.
    harness.landAt(terminal("@2", []));
    expect(harness.hook.history.entries.map((entry) => entry.appTabId)).toEqual([undefined, undefined]);

    act(() => harness.hook.step("back"));
    expect(harness.selectAppTab).not.toHaveBeenCalled();
    expect(harness.selectWindow).toHaveBeenCalledExactlyOnceWith("@1");
    harness.landAt(terminal("@1", []));
    expect(harness.hook.canGoBack).toBe(false);
    harness.unmount();
  });

  it("reaches a document in another workspace by switching the workspace first", () => {
    const remote = tab("remote", "$2");
    const harness = mount(document("remote", "@3", [remote], "$2"));
    harness.landAt(terminal("@1", [remote]));
    act(() => harness.hook.step("back"));
    expect(harness.selectSession).toHaveBeenCalledExactlyOnceWith("$2");
    expect(harness.selectAppTab).toHaveBeenCalledExactlyOnceWith("$2", "@3", "remote");
    harness.landAt(document("remote", "@3", [remote], "$2"));
    expect(harness.hook.history.entries).toHaveLength(2);
    expect(harness.hook.history.cursor).toBe(0);
    harness.unmount();
  });

  it("records a traversal that landed somewhere else as the visit it was", () => {
    const harness = mount(terminal("@1"));
    harness.landAt(terminal("@2"));
    act(() => harness.hook.step("back"));
    expect(harness.selectWindow).toHaveBeenCalledExactlyOnceWith("@1");
    // The host refused the switch and tmux moved the client to "@3" instead.
    harness.landAt(terminal("@3", [], "$2"));
    expect(harness.hook.history.entries.map((entry) => entry.windowId)).toEqual(["@1", "@3"]);
    expect(harness.hook.history.cursor).toBe(1);
    harness.unmount();
  });
});

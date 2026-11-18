// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { useWorkspaceRestore } from "./useWorkspaceRestore";
import type { TmuxSnapshot } from "./types";
import { emptyHostLink, type HostLink } from "../state/hostLinks";
import { connectionReducer, initialHostState, type HostAction } from "../state/connectionReducer";
import { defaultAppState, type PersistedAppState } from "../features/shell/types";
import type { TmuxActionResult } from "../features/tmux/actions";

const session = (id: string, name: string) => ({ id, name, windowCount: 1, attachedClients: 0 });
const tab = (id: string, sessionId: string, index: number) => ({
  id, sessionId, index, name: `w${index}`, active: false, layout: "",
});
const pane = (id: string, sessionId: string, windowId: string, currentPath: string) => ({
  id, sessionId, windowId, index: 0, active: true, width: 80, height: 24, left: 0, top: 0,
  currentPath, currentCommand: "bash",
});

/** A live link whose host has reported `snapshot` and is connected. */
function connectedLink(snapshot: TmuxSnapshot, activeSessionId?: string): HostLink {
  const actions: HostAction[] = [
    { type: "connection", phase: "connected" },
    { type: "snapshot", snapshot, sequence: 1, serverIdentity: "server-a", generation: 1 },
  ];
  const hostState = actions.reduce((state, action) => connectionReducer(state, action), initialHostState);
  return { ...emptyHostLink("local", { mode: "local" }, 0), hostState, activeSessionId };
}

type Options = Parameters<typeof useWorkspaceRestore>[0];

interface Drive {
  enabled?: boolean;
  initialAppState?: PersistedAppState;
  initialLink: HostLink;
  performAction: Options["performAction"];
  rerenderLink?: HostLink;
}

/**
 * Mounts the hook over one connected local link, optionally rerenders with a
 * new link, and reports what the hook asked the host to do and what it wrote
 * back into the persisted state.
 */
async function drive(options: Drive) {
  // One stable ref object for the hook's reads and the assertions alike: the
  // app-state ref, the state itself, and the `setAppState` that joins them.
  const appStateRef = { current: options.initialAppState ?? defaultAppState };
  const setAppState = vi.fn<Options["setAppState"]>((update) => {
    appStateRef.current = typeof update === "function"
      ? (update as (current: PersistedAppState) => PersistedAppState)(appStateRef.current)
      : update;
  });
  const selectSessionOnActiveHost = vi.fn();

  function Harness(props: { links: Options["links"] }) {
    useWorkspaceRestore({
      appStateLoaded: true,
      enabled: options.enabled ?? true,
      links: props.links,
      activeHostProfileId: "local",
      linkFor: (profileId) => (profileId === "local" ? props.links[0] : undefined),
      appStateRef,
      setAppState,
      performAction: options.performAction,
      targetFor: () => undefined,
      selectSessionOnActiveHost,
    });
    return null;
  }

  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness links={[options.initialLink]} />); });
  if (options.rerenderLink) {
    await act(async () => { renderer.update(<Harness links={[options.rerenderLink!]} />); });
  }
  await act(async () => renderer.unmount());
  return { setAppState, selectSessionOnActiveHost, state: () => appStateRef.current };
}

const ack = (extra: Partial<TmuxActionResult>): TmuxActionResult =>
  ({ topologyGeneration: 2, ...extra });

describe("restoring remembered workspaces", () => {
  const remembered: PersistedAppState = {
    ...defaultAppState,
    shell: { ...defaultAppState.shell, restoreWorkspaces: true },
    workspaceRestore: {
      local: {
        workspaces: [
          { name: "alpha", tabs: [{ directory: "/a" }, {}] },
          { name: "beta", tabs: [{ directory: "/b" }] },
        ],
        selectedSessionName: "alpha",
      },
    },
  };

  it("creates the missing workspace at its remembered directories and leaves the open one alone", async () => {
    const performAction = vi.fn<Options["performAction"]>(async (action) =>
      action.kind === "createSession" ? ack({ sessionId: "$9", windowId: "@9" }) : ack({ windowId: "@10" }));
    const open = { sessions: [session("$2", "beta")], windows: [], panes: [] };
    const result = await drive({
      initialAppState: remembered, initialLink: connectedLink(open, "$2"), performAction,
    });
    // beta is open by name, so only alpha is created: first tab through the
    // session create, the remembered second tab as an explicit new-window.
    expect(performAction.mock.calls.map(([action]) => action)).toEqual([
      { kind: "createSession", name: "alpha", directory: "/a" },
      { kind: "createWindow", sessionId: "$9" },
    ]);
    // The remembered selection was just created and its snapshot has not
    // landed; the ack's id is the earliest truth, and selection uses it.
    expect(result.selectSessionOnActiveHost).toHaveBeenCalledWith("$9");
  });

  it("selects a remembered workspace that was already open without creating anything", async () => {
    const performAction = vi.fn<Options["performAction"]>(async () => ack({}));
    const open = { sessions: [session("$2", "beta")], windows: [], panes: [] };
    const result = await drive({
      initialAppState: {
        ...remembered,
        workspaceRestore: { local: { workspaces: [{ name: "beta", tabs: [{ directory: "/b" }] }], selectedSessionName: "beta" } },
      },
      initialLink: connectedLink(open, "$1"),
      performAction,
    });
    expect(performAction).not.toHaveBeenCalled();
    expect(result.selectSessionOnActiveHost).toHaveBeenCalledWith("$2");
  });

  it("restores once per link, not once per render", async () => {
    const performAction = vi.fn<Options["performAction"]>(async () => ack({ sessionId: "$9" }));
    const empty = { sessions: [], windows: [], panes: [] };
    const link = connectedLink(empty);
    // The rerender is a fresh array for the same link: the second pass must
    // find the epoch already settled and ask for nothing.
    await drive({
      initialAppState: { ...remembered, workspaceRestore: { local: { workspaces: [{ name: "alpha", tabs: [] }] } } },
      initialLink: link,
      rerenderLink: connectedLink(empty),
      performAction,
    });
    expect(performAction).toHaveBeenCalledTimes(1);
  });

  it("waits for the host and does nothing while the setting is off", async () => {
    const performAction = vi.fn<Options["performAction"]>(async () => ack({ sessionId: "$9" }));
    const off = { ...defaultAppState, shell: { ...defaultAppState.shell, restoreWorkspaces: false } };
    await drive({
      enabled: false,
      initialAppState: { ...off, workspaceRestore: remembered.workspaceRestore },
      initialLink: connectedLink({ sessions: [], windows: [], panes: [] }),
      performAction,
    });
    expect(performAction).not.toHaveBeenCalled();
  });
});

describe("capturing the record as the snapshot moves", () => {
  it("writes what is open into the persisted state after the restore settles", async () => {
    const performAction = vi.fn<Options["performAction"]>(async () => ack({}));
    const open: TmuxSnapshot = {
      sessions: [session("$1", "alpha")],
      windows: [tab("@1", "$1", 0)],
      panes: [pane("%1", "$1", "@1", "/work")],
    };
    const result = await drive({
      initialLink: connectedLink(open, "$1"),
      rerenderLink: connectedLink(open, "$1"),
      performAction,
    });
    expect(result.state().workspaceRestore).toEqual({
      local: { workspaces: [{ name: "alpha", tabs: [{ directory: "/work" }] }], selectedSessionName: "alpha" },
    });
  });

  it("does not capture over the record while a restore is still in flight", async () => {
    // A fresh tmux server's first snapshot is the empty one. Capturing it now
    // would delete the very record the pending restore is acting on.
    const performAction = vi.fn<Options["performAction"]>(() => new Promise(() => undefined));
    const result = await drive({
      initialAppState: {
        ...defaultAppState,
        shell: { ...defaultAppState.shell, restoreWorkspaces: true },
        workspaceRestore: { local: { workspaces: [{ name: "alpha", tabs: [{ directory: "/a" }] }] } },
      },
      initialLink: connectedLink({ sessions: [], windows: [], panes: [] }),
      rerenderLink: connectedLink({ sessions: [], windows: [], panes: [] }),
      performAction,
    });
    expect(result.setAppState).not.toHaveBeenCalled();
    expect(result.state().workspaceRestore.local.workspaces).toHaveLength(1);
  });

  it("leaves a host with nothing open without a record", async () => {
    const performAction = vi.fn<Options["performAction"]>(async () => ack({}));
    const empty = { sessions: [], windows: [], panes: [] };
    const result = await drive({
      initialLink: connectedLink(empty),
      rerenderLink: connectedLink(empty),
      performAction,
    });
    expect(result.state().workspaceRestore).toEqual({});
  });
});

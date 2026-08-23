// @vitest-environment jsdom
import { useRef } from "react";
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { useBulkTabClose } from "./useBulkTabClose";
import type { TmuxSnapshot, Window as TmuxWindow } from "./types";
import type { HostScopeToken } from "../features/shell/hostScope";
import { combineWorkspaceTabs, tabsToCloseNonAgent, type AgentPresenceSnapshot } from "../features/shell/model";
import type { AgentAttentionRollup } from "../features/agents/types";
import type { AppOwnedTab } from "../features/shell/types";

const windows: TmuxWindow[] = [
  { id: "@1", sessionId: "$1", index: 1, name: "shell", active: true, layout: "" },
  { id: "@2", sessionId: "$1", index: 2, name: "worker", active: false, layout: "" },
];
const snapshot: TmuxSnapshot = { sessions: [], windows, panes: [] };
const appTab: AppOwnedTab = {
  id: "tab-1", hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1",
  sessionName: "muxflow", kind: "file", resource: "/notes.md", title: "notes.md", order: 0,
};
const scope: HostScopeToken = {
  hostProfileId: "local", connectionKey: "local", connectionEpoch: 1, serverIdentity: "server-a", generation: 8,
};
const authority = {
  hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, topologyGeneration: 8,
};
const covering = { ...authority, coveredWindowIds: new Set(["@1", "@2"]) };

type Options = Parameters<typeof useBulkTabClose>[0];

function presence(byWindow: ReadonlyMap<string, AgentAttentionRollup> = new Map()): AgentPresenceSnapshot {
  return { accepted: covering, current: authority, byWindow };
}

function agentIn(windowId: string): ReadonlyMap<string, AgentAttentionRollup> {
  return new Map([[windowId, {
    state: "working", adapterId: "codex", blocked: 0, working: 1, done: 0, unknown: 0, idle: 0, total: 1,
  } satisfies AgentAttentionRollup]]);
}

/**
 * Drives the hook once over the tabs the strip would have offered.
 *
 * `menuPresence` builds the set — what the person saw when they picked the
 * item — and `commitPresence` is what the recheck reads, which is how an agent
 * that appeared in between is expressed.
 */
async function closeSet(options: {
  commitPresence?: AgentPresenceSnapshot;
  liveScope?: HostScopeToken;
  menuPresence?: AgentPresenceSnapshot;
} = {}) {
  const closeAppTab = vi.fn();
  const setStatus = vi.fn<(status: string) => void>();
  // Typed against the real option, so the assertions below read the same
  // argument shape the hook actually dispatches.
  const performAction = vi.fn<Options["performAction"]>(async () => ({ topologyGeneration: 8 }));
  const tabs = tabsToCloseNonAgent(combineWorkspaceTabs(
    windows, [appTab], options.menuPresence?.byWindow ?? new Map(), undefined, presence(),
  ));
  let call: ReturnType<typeof useBulkTabClose> | undefined;

  function Harness() {
    const closeTabSet = useBulkTabClose({
      agentPresenceRef: useRef(options.commitPresence ?? presence()),
      closeAppTab,
      hostScopeRef: useRef(options.liveScope ?? scope),
      performAction,
      setStatus,
      snapshotRef: useRef(snapshot),
      workspaceAppTabs: [appTab],
    });
    call = closeTabSet;
    return null;
  }

  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness />); });
  await act(async () => { await call!(tabs, scope, true); });
  await act(async () => renderer.unmount());
  return { closeAppTab, performAction, setStatus, tabs };
}

describe("bulk tab close", () => {
  it("survives the topology generation moving under an open confirmation", async () => {
    // An agent animating its title bumps the generation several times a second,
    // so a set captured before the dialog can never match it again. The durable
    // connection is what has to hold.
    const result = await closeSet({ liveScope: { ...scope, generation: 41 } });
    expect(result.performAction).toHaveBeenCalledTimes(2);
    expect(result.closeAppTab).toHaveBeenCalledTimes(1);
    expect(result.setStatus).not.toHaveBeenCalled();
  });

  it("stops at a tab that gained an agent, keeping its terminals and its editors", async () => {
    // Nothing had an agent when the menu was opened; one arrived before the
    // commit. The recheck holds that terminal back, and a set that could not be
    // closed whole must not lose its file tabs either.
    const result = await closeSet({ commitPresence: presence(agentIn("@2")) });
    expect(result.performAction).toHaveBeenCalledTimes(1);
    expect(result.performAction.mock.calls[0]?.[0]).toMatchObject({ kind: "closeWindow", windowId: "@1" });
    expect(result.closeAppTab).not.toHaveBeenCalled();
    expect(result.setStatus)
      .toHaveBeenCalledWith("Closed 1 of 2 tabs; 1 still had an agent and was left open.");
  });

  it("cancels when the durable host connection is replaced", async () => {
    const result = await closeSet({ liveScope: { ...scope, serverIdentity: "server-b" } });
    expect(result.performAction).not.toHaveBeenCalled();
    expect(result.closeAppTab).not.toHaveBeenCalled();
    expect(result.setStatus).toHaveBeenCalledWith("Closing those tabs was cancelled because its host scope changed.");
  });
});

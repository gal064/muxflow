import { describe, expect, it } from "vitest";
import type { TmuxSnapshot } from "../../app/types";
import { agent } from "../agents/testFixtures";
import { deriveAgentRollups } from "../agents/selectors";
import type { HostScopeToken } from "../shell/hostScope";
import { hostWorkspaceRows, mergeHostRows, mergedWorkspaceRows, pinnedOnlyMergedRows, type HostRowSource } from "./mergedWorkspaceRows";
import { workspaceRows } from "./workspaceRows";

const scopeFor = (hostProfileId: string): HostScopeToken =>
  ({ hostProfileId, connectionKey: `ssh:${hostProfileId}`, connectionEpoch: 1, serverIdentity: `server-${hostProfileId}`, generation: 1 });

/** A host with these sessions, `order` in the order given, pinned where named. */
function source(hostProfileId: string, letter: string, sessions: readonly { id: string; name: string; pinned?: boolean }[], extra: Partial<HostRowSource> = {}): HostRowSource {
  const snapshot: TmuxSnapshot = {
    sessions: sessions.map((session, order) => ({ id: session.id, name: session.name, windowCount: 1, attachedClients: 0, order, pinned: session.pinned })),
    windows: [],
    panes: [],
  };
  return {
    hostProfileId, letter, label: hostProfileId, phase: "connected", canMutate: true, transport: "ssh", scope: scopeFor(hostProfileId),
    snapshot, agents: [], adapters: [], attentionByWorkspace: new Map(), ...extra,
  };
}

const local = source("local", "L", [
  { id: "$0", name: "alpha" },
  { id: "$1", name: "beta", pinned: true },
  { id: "$2", name: "gamma" },
], { activeSessionId: "$0" });
const peer = source("peer", "P", [
  { id: "$0", name: "delta", pinned: true },
  { id: "$1", name: "epsilon" },
]);

const names = (rows: readonly { hostProfileId: string; session: { name: string } }[]) =>
  rows.map((row) => `${row.hostProfileId}:${row.session.name}`);

describe("mergedWorkspaceRows", () => {
  it("puts every pinned row first, sources in order and each host's own order inside", () => {
    expect(names(mergedWorkspaceRows([local, peer], true))).toEqual([
      "local:beta", "peer:delta",
      "local:alpha", "local:gamma", "peer:epsilon",
    ]);
    // The sources' order is the tie-break at both levels, not the hosts' names.
    expect(names(mergedWorkspaceRows([peer, local], true))).toEqual([
      "peer:delta", "local:beta",
      "peer:epsilon", "local:alpha", "local:gamma",
    ]);
  });

  it("keys the same session id on two hosts apart, and tags each row with its host", () => {
    const rows = mergedWorkspaceRows([local, peer], true);
    const zeros = rows.filter((row) => row.session.id === "$0");
    expect(zeros).toHaveLength(2);
    expect(new Set(rows.map((row) => row.key)).size).toBe(rows.length);
    // Peer first: its `$0` is pinned, and pinned rows lead the list.
    expect(zeros.map((row) => row.key)).toEqual(["peer\0$0", "local\0$0"]);
    expect(zeros.map((row) => row.scope.hostProfileId)).toEqual(["peer", "local"]);
    expect(zeros.map((row) => row.canMutate)).toEqual([true, true]);
    // Active is the active host's business alone: `$0` on the peer is not it.
    expect(zeros.map((row) => row.active)).toEqual([false, true]);
  });

  it("merges per-host lists the caller built the same way it builds them", () => {
    expect(mergeHostRows([hostWorkspaceRows(local, true), hostWorkspaceRows(peer, true)]))
      .toEqual(mergedWorkspaceRows([local, peer], true));
  });

  it("carries the host letter only when asked to", () => {
    expect(mergedWorkspaceRows([local, peer], true).map((row) => row.letter)).toEqual(["L", "P", "L", "L", "P"]);
    expect(mergedWorkspaceRows([local, peer], false).every((row) => row.letter === "")).toBe(true);
  });

  it("carries a read-only host's flag onto its rows", () => {
    const rows = mergedWorkspaceRows([local, { ...peer, canMutate: false }], false);
    expect(rows.filter((row) => row.hostProfileId === "peer").every((row) => !row.canMutate)).toBe(true);
    expect(rows.filter((row) => row.hostProfileId === "local").every((row) => row.canMutate)).toBe(true);
  });

  it("reproduces workspaceRows for a single source, agents and all", () => {
    const agents = [
      agent({ id: "a", hostProfileId: "local", sessionId: "$2", displayName: "claude", lifecycle: "blocked" }),
      agent({ id: "b", hostProfileId: "local", sessionId: "$0", displayName: "codex", lifecycle: "working" }),
    ];
    const inputs = {
      snapshot: local.snapshot, activeSessionId: "$0", agents, adapters: [],
      attentionByWorkspace: deriveAgentRollups(agents).byWorkspace, activeBranch: "main", home: "/home/operator",
    };
    const plain = workspaceRows(inputs);
    const merged = mergedWorkspaceRows([{ ...local, ...inputs }], false);
    expect(merged.map(({ key: _key, hostProfileId: _host, letter: _letter, scope: _scope, phase: _phase, canMutate: _canMutate, ...row }) => row)).toEqual(plain);
    expect(merged[0].attention).toBe("none");
    expect(merged[2].agents.map((item) => item.name)).toEqual(["claude"]);
  });
});

describe("pinnedOnlyMergedRows", () => {
  it("keeps pinned rows everywhere and the active row on the active host only", () => {
    const rows = mergedWorkspaceRows([local, peer], true);
    // `$0` is active on local and merely present on peer.
    expect(names(pinnedOnlyMergedRows(rows, { hostProfileId: "local", sessionId: "$0" })))
      .toEqual(["local:beta", "peer:delta", "local:alpha"]);
    // Switched to the peer: its unpinned `$1` stays, local's `$1` is pinned anyway.
    expect(names(pinnedOnlyMergedRows(rows, { hostProfileId: "peer", sessionId: "$1" })))
      .toEqual(["local:beta", "peer:delta", "peer:epsilon"]);
    expect(names(pinnedOnlyMergedRows(rows, { hostProfileId: "local" })))
      .toEqual(["local:beta", "peer:delta"]);
  });
});

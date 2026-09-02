import { describe, expect, it } from "vitest";
import type { ConnectionSpec, HostProfile, TmuxSnapshot } from "../app/types";
import { initialHostState } from "./connectionReducer";
import {
  hostLinksReducer,
  initialHostLinksState,
  shownHostProfiles,
  type HostLinksAction,
  type HostLinksState,
} from "./hostLinks";

const local: ConnectionSpec = { mode: "local" };
const remote = (profileId: string, target = `${profileId}-host`): ConnectionSpec => ({ mode: "ssh", profileId, target });

function reduce(actions: HostLinksAction[], state: HostLinksState = initialHostLinksState): HostLinksState {
  return actions.reduce(hostLinksReducer, state);
}

const twoHosts = () => reduce([
  { type: "sync", hosts: [{ profileId: "local", connection: local }, { profileId: "qa", connection: remote("qa") }] },
]);

const snapshot = (sessions: Array<[id: string, name: string]>): TmuxSnapshot => ({
  sessions: sessions.map(([id, name]) => ({ id, name, windowCount: 1, attachedClients: 0 })),
  windows: [],
  panes: [],
});

const snapshotAction = (profileId: string, world: TmuxSnapshot, generation = 1): HostLinksAction => ({
  type: "host",
  profileId,
  action: { type: "snapshot", snapshot: world, sequence: 0, generation, serverIdentity: "srv" },
});

describe("the host link set", () => {
  it("follows the shown hosts in their order, each link born empty under its own epoch", () => {
    const state = twoHosts();
    expect(state.order).toEqual(["local", "qa"]);
    expect(state.byProfileId.local).toMatchObject({
      profileId: "local", connection: local, connectionEpoch: 0, terminalEpoch: 0, hostState: initialHostState, detail: "",
    });
    expect(state.byProfileId.qa.connectionEpoch).toBe(1);
    expect(state.byProfileId.qa.clientId).toBeUndefined();
  });

  it("is untouched by a sync that changes nothing, and keeps a surviving link's state across one that does", () => {
    const state = reduce([{ type: "client", profileId: "qa", clientId: "client-2" }], twoHosts());
    const same = hostLinksReducer(state, {
      type: "sync", hosts: [{ profileId: "local", connection: { mode: "local" } }, { profileId: "qa", connection: remote("qa") }],
    });
    expect(same).toBe(state);

    const reordered = hostLinksReducer(state, {
      type: "sync", hosts: [{ profileId: "qa", connection: remote("qa") }, { profileId: "staging", connection: remote("staging") }],
    });
    expect(reordered.order).toEqual(["qa", "staging"]);
    expect(reordered.byProfileId.local).toBeUndefined();
    // The link that stayed is the same object: nothing keyed on it recomputes.
    expect(reordered.byProfileId.qa).toBe(state.byProfileId.qa);
    expect(reordered.byProfileId.staging.connectionEpoch).toBe(2);
  });

  it("moves a link onto a corrected address without minting an epoch", () => {
    const state = twoHosts();
    const moved = hostLinksReducer(state, {
      type: "sync", hosts: [{ profileId: "local", connection: local }, { profileId: "qa", connection: remote("qa", "qa-2") }],
    });
    expect(moved.byProfileId.qa.connection).toEqual(remote("qa", "qa-2"));
    expect(moved.byProfileId.qa.connectionEpoch).toBe(1);
    expect(moved.byProfileId.local).toBe(state.byProfileId.local);
  });

  it("never hands a profile an epoch it has had before, even after it leaves and comes back", () => {
    let state = twoHosts();
    const used = new Set<number>([state.byProfileId.qa.connectionEpoch]);
    state = hostLinksReducer(state, { type: "reconnect", profileId: "qa" });
    expect(used.has(state.byProfileId.qa.connectionEpoch)).toBe(false);
    used.add(state.byProfileId.qa.connectionEpoch);
    expect(state.byProfileId.local.connectionEpoch).toBe(0);

    state = hostLinksReducer(state, { type: "remove", profileId: "qa" });
    state = hostLinksReducer(state, { type: "add", profileId: "qa", connection: remote("qa") });
    expect(used.has(state.byProfileId.qa.connectionEpoch)).toBe(false);
    expect(state.order).toEqual(["local", "qa"]);
  });

  it("reconnects every link at once with distinct fresh epochs", () => {
    const state = hostLinksReducer(twoHosts(), { type: "reconnectAll" });
    const epochs = state.order.map((id) => state.byProfileId[id].connectionEpoch);
    expect(new Set(epochs).size).toBe(2);
    expect(Math.min(...epochs)).toBeGreaterThan(1);
  });

  it("records a link's client, native epoch and detail, and ignores a host it does not know", () => {
    const state = reduce([
      { type: "client", profileId: "qa", clientId: "client-2" },
      { type: "terminalEpoch", profileId: "qa", terminalEpoch: 7 },
      { type: "detail", profileId: "qa", detail: "Connection refused" },
      { type: "client", profileId: "ghost", clientId: "client-9" },
    ], twoHosts());
    expect(state.byProfileId.qa).toMatchObject({ clientId: "client-2", terminalEpoch: 7, detail: "Connection refused" });
    expect(state.byProfileId.ghost).toBeUndefined();
    expect(hostLinksReducer(state, { type: "detail", profileId: "qa", detail: "Connection refused" })).toBe(state);
  });

  it("reduces host state per link, and a snapshot lands the link on a remembered session", () => {
    let state = reduce([
      { type: "host", profileId: "qa", action: { type: "connection", phase: "connected" } },
      snapshotAction("qa", snapshot([["$0", "home"], ["$1", "work"]])),
    ], twoHosts());
    expect(state.byProfileId.qa.hostState.phase).toBe("connected");
    expect(state.byProfileId.qa.activeSessionId).toBe("$0");
    expect(state.byProfileId.local.hostState).toBe(initialHostState);

    state = hostLinksReducer(state, { type: "activeSession", profileId: "qa", sessionId: "$1" });
    // A rename keeps the session; the id is what the user is on.
    state = hostLinksReducer(state, snapshotAction("qa", snapshot([["$0", "home"], ["$1", "renamed"]]), 2));
    expect(state.byProfileId.qa.activeSessionId).toBe("$1");
    // A recreated session with the remembered name is found by that name.
    state = hostLinksReducer(state, snapshotAction("qa", snapshot([["$0", "home"], ["$5", "renamed"]]), 3));
    expect(state.byProfileId.qa.activeSessionId).toBe("$5");
    // Gone entirely, the first session is where the host lands.
    state = hostLinksReducer(state, snapshotAction("qa", snapshot([["$0", "home"]]), 4));
    expect(state.byProfileId.qa.activeSessionId).toBe("$0");
  });

  it("takes a session or window as a value or as an updater, and stays put when nothing changes", () => {
    let state = hostLinksReducer(twoHosts(), { type: "activeWindow", profileId: "local", windowId: "@1" });
    expect(state.byProfileId.local.activeWindowId).toBe("@1");
    state = hostLinksReducer(state, { type: "activeWindow", profileId: "local", windowId: (current) => `${current}-next` });
    expect(state.byProfileId.local.activeWindowId).toBe("@1-next");
    expect(hostLinksReducer(state, { type: "activeWindow", profileId: "local", windowId: (current) => current })).toBe(state);
    state = hostLinksReducer(state, { type: "activeSession", profileId: "local", sessionId: (current) => current ?? "$3" });
    expect(state.byProfileId.local.activeSessionId).toBe("$3");
  });
});

describe("which hosts are shown", () => {
  const profiles: HostProfile[] = [
    { id: "local", label: "Local", connection: local, shown: true },
    { id: "qa", label: "qa", connection: remote("qa", "qa-host"), shown: true },
    { id: "prod", label: "prod", connection: remote("prod") },
  ];

  it("lists the checked profiles in profile order, under the connection each is opened with", () => {
    expect(shownHostProfiles(profiles, local)).toEqual([
      { profileId: "local", connection: local },
      { profileId: "qa", connection: remote("qa", "qa-host") },
    ]);
  });

  it("always includes the active host, under the live connection, whether or not it is checked", () => {
    const editing: ConnectionSpec = { mode: "ssh", profileId: "prod", target: "prod-host-2" };
    expect(shownHostProfiles(profiles, editing)).toEqual([
      { profileId: "local", connection: local },
      { profileId: "qa", connection: remote("qa", "qa-host") },
      { profileId: "prod", connection: editing },
    ]);
    const unsaved: ConnectionSpec = { mode: "ssh", profileId: "new", target: "new-host" };
    expect(shownHostProfiles(profiles, unsaved)[0]).toEqual({ profileId: "new", connection: unsaved });
  });
});

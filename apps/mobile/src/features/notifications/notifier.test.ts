import { beforeEach, describe, expect, it } from "vitest";

import type { AgentNotification, NotificationHost } from "./host";
import { createAgentNotifier, type AgentNotifier } from "./notifier";
import { initialSessionState, type Agent, type AgentTransition, type SessionState } from "../../store/sessionStore";

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "a1",
    adapterId: "claude-code",
    displayName: "Claude",
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 1n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 0,
    present: true,
    route: { sessionId: "$1", sessionNameFallback: "", windowId: "@1", windowNameFallback: "", paneId: "%1", paneIndexFallback: 0 },
    ...overrides,
  };
}

const blocked = (overrides: Partial<Agent> = {}) =>
  agent({ lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: 2n, ...overrides });
const completed = (overrides: Partial<Agent> = {}) =>
  agent({ lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2n, ...overrides });

/** A tray keyed by tag, the way Android keys notifications by tag. */
function fakeHost() {
  const presented: AgentNotification[] = [];
  const cancelled: string[] = [];
  const tray = new Map<string, AgentNotification>();
  const host: NotificationHost = {
    ensureChannel: async () => {},
    getPermission: async () => "granted",
    requestPermission: async () => "granted",
    present: async (notification) => {
      presented.push(notification);
      tray.set(notification.tag, notification);
    },
    cancel: async (tag) => {
      cancelled.push(tag);
      tray.delete(tag);
    },
    onTap: () => () => {},
  };
  return { host, presented, cancelled, tray };
}

/** A store stand-in: the state is set outright and `emit()` is the subscription. */
function harness(options: { foreground?: boolean } = {}) {
  const platform = fakeHost();
  let state: SessionState = initialSessionState();
  const storeListeners = new Set<() => void>();
  const transitionListeners = new Set<(transition: AgentTransition) => void>();
  const notifier: AgentNotifier = createAgentNotifier({
    host: platform.host,
    getState: () => state,
    subscribe: (listener) => {
      storeListeners.add(listener);
      return () => storeListeners.delete(listener);
    },
    onAgentTransition: (listener) => {
      transitionListeners.add(listener);
      return () => transitionListeners.delete(listener);
    },
    appInForeground: () => options.foreground ?? false,
  });
  notifier.start();

  const setState = (patch: Partial<SessionState>): void => {
    state = { ...state, ...patch };
    for (const listener of storeListeners) listener();
  };
  const put = (...agents: Agent[]): void => {
    setState({ agents: { ...state.agents, ...Object.fromEntries(agents.map((a) => [a.id, a])) } });
  };
  const transition = async (prev: Agent | undefined, next: Agent): Promise<void> => {
    state = { ...state, agents: { ...state.agents, [next.id]: next } };
    for (const listener of transitionListeners) listener({ prev, next });
    for (const listener of storeListeners) listener();
    await notifier.settled();
  };
  const settle = async (): Promise<void> => {
    await notifier.settled();
  };
  return { ...platform, notifier, setState, put, transition, settle, state: () => state };
}

describe("agent notifier (§13)", () => {
  let h: ReturnType<typeof harness>;
  beforeEach(() => {
    h = harness();
  });

  it("posts §13's exact title and body when an agent becomes blocked", async () => {
    h.setState({ sessions: { $1: { id: "$1", name: "muxflow", windowCount: 1, order: 0 } } });
    await h.transition(agent(), blocked());
    expect(h.presented).toEqual([{
      tag: "a1",
      title: "muxflow · Claude",
      body: "Needs your input",
      data: { agentId: "a1", paneId: "%1", sessionId: "$1", attentionGeneration: "2" },
    }]);
  });

  it("falls back to the route's workspace name when the topology has no session", async () => {
    await h.transition(agent(), blocked({ route: { ...agent().route, sessionNameFallback: "muxflow" } }));
    expect(h.presented[0]?.title).toBe("muxflow · Claude");
  });

  it("posts `Finished` for the working → idle completion", async () => {
    await h.transition(agent({ lifecycle: "working", attentionGeneration: 1n }), completed());
    expect(h.presented).toHaveLength(1);
    expect(h.presented[0]?.body).toBe("Finished");
  });

  it("does not post below the notification watermark (step 4)", async () => {
    h.setState({ notificationWatermark: 5n });
    await h.transition(agent({ attentionGeneration: 1n }), blocked({ attentionGeneration: 5n }));
    expect(h.presented).toHaveLength(0);
  });

  it("posts once per (agent, generation), even when the same state is replayed (step 5)", async () => {
    const first = blocked();
    await h.transition(agent(), first);
    // A reconciling snapshot re-delivers the same record with a stale `prev`.
    await h.transition(agent(), blocked());
    expect(h.presented).toHaveLength(1);
  });

  it("coalesces: a newer generation replaces the older notification under one tag", async () => {
    await h.transition(agent(), blocked({ attentionGeneration: 2n }));
    // Answered on the desktop, then blocked again.
    await h.transition(blocked({ attentionGeneration: 2n, seenGeneration: 2n }), blocked({ attentionGeneration: 3n, seenGeneration: 2n }));
    expect(h.presented.map((n) => n.tag)).toEqual(["a1", "a1"]);
    expect(h.presented.map((n) => n.data.attentionGeneration)).toEqual(["2", "3"]);
    expect([...h.tray.keys()]).toEqual(["a1"]);
    expect(h.tray.get("a1")?.data.attentionGeneration).toBe("3");
  });

  it("keeps agents apart: one tag each", async () => {
    await h.transition(agent(), blocked());
    await h.transition(agent({ id: "a2" }), blocked({ id: "a2", displayName: "Codex" }));
    expect([...h.tray.keys()].sort()).toEqual(["a1", "a2"]);
  });

  describe("step 6, the focused pane", () => {
    it("stays quiet for the pane on screen while the app is in the foreground", async () => {
      const front = harness({ foreground: true });
      front.setState({ focusedPaneId: "%1" });
      await front.transition(agent(), blocked());
      expect(front.presented).toHaveLength(0);
    });

    it("still posts for that pane when the app is in the background", async () => {
      const back = harness({ foreground: false });
      back.setState({ focusedPaneId: "%1" });
      await back.transition(agent(), blocked());
      expect(back.presented).toHaveLength(1);
    });

    it("posts for another pane even in the foreground", async () => {
      const front = harness({ foreground: true });
      front.setState({ focusedPaneId: "%9" });
      await front.transition(agent(), blocked());
      expect(front.presented).toHaveLength(1);
    });
  });

  describe("cancelling", () => {
    it("cancels by tag once the agent is marked seen", async () => {
      await h.transition(agent(), blocked());
      expect(h.cancelled).toEqual([]);
      h.put(blocked({ seenGeneration: 2n }));
      await h.settle();
      expect(h.cancelled).toEqual(["a1"]);
      expect(h.tray.size).toBe(0);
    });

    it("cancels when a blocked agent goes back to work without being seen", async () => {
      await h.transition(agent(), blocked());
      h.put(blocked({ lifecycle: "working" }));
      await h.settle();
      expect(h.cancelled).toEqual(["a1"]);
    });

    it("cancels when the agent is retired or goes away", async () => {
      await h.transition(agent(), blocked());
      h.setState({ agents: {} });
      await h.settle();
      expect(h.cancelled).toEqual(["a1"]);

      const gone = harness();
      await gone.transition(agent(), blocked());
      gone.put(blocked({ present: false }));
      await gone.settle();
      expect(gone.cancelled).toEqual(["a1"]);
    });

    it("keeps a `Finished` notification while the agent sits idle, and drops it when seen", async () => {
      await h.transition(agent({ attentionGeneration: 1n }), completed());
      h.put(completed());
      await h.settle();
      expect(h.cancelled).toEqual([]);
      h.put(completed({ seenGeneration: 2n }));
      await h.settle();
      expect(h.cancelled).toEqual(["a1"]);
    });

    it("cancels once, not on every later store change", async () => {
      await h.transition(agent(), blocked());
      h.put(blocked({ seenGeneration: 2n }));
      h.put(blocked({ seenGeneration: 2n }));
      await h.settle();
      expect(h.cancelled).toEqual(["a1"]);
      expect(h.notifier.outstandingTags()).toEqual([]);
    });

    it("does not cancel a blocked notification that had nothing unseen behind it", async () => {
      // §13 step 3's second branch: a fresh `blocked` lifecycle with no
      // attention advance. `needsAttention` is already false, and the
      // notification must survive its own posting.
      const seen = blocked({ attentionGeneration: 4n, seenGeneration: 4n });
      await h.transition(agent({ attentionGeneration: 4n, seenGeneration: 4n }), seen);
      expect(h.presented).toHaveLength(1);
      h.put(seen);
      await h.settle();
      expect(h.cancelled).toEqual([]);
    });
  });

  it("stops listening after stop()", async () => {
    h.notifier.stop();
    await h.transition(agent(), blocked());
    expect(h.presented).toHaveLength(0);
  });
});

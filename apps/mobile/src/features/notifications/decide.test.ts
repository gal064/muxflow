import { describe, expect, it } from "vitest";
import { decideAgentNotification, type NotificationContext } from "./decide";
import type { Agent } from "../../store/sessionStore";

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "a1",
    adapterId: "claude-code",
    nativeSessionId: "",
    displayName: "Claude",
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 1n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 0,
    lifecycleChangedAtMs: 0,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId: "$1", sessionNameFallback: "", windowId: "@1", windowNameFallback: "", paneId: "%1", paneIndexFallback: 0 },
    ...overrides,
  };
}

function context(overrides: Partial<NotificationContext> = {}): NotificationContext {
  return {
    notificationWatermark: 0n,
    alreadyNotified: () => false,
    focusedPaneId: undefined,
    viewedAgentId: undefined,
    appInForeground: false,
    workspaceName: "muxflow",
    agentName: "Fix tests",
    ...overrides,
  };
}

const blocked = (gen: bigint) => agent({ lifecycle: "blocked", attentionKind: "blocked", attentionGeneration: gen });
const completed = (gen: bigint) => agent({ lifecycle: "idle", attentionKind: "completed", attentionGeneration: gen });

describe("notification decision rule (§13), one case per step", () => {
  it("step 3: attention advanced → attentionKind decides; empty kind → nothing", () => {
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), context())).toMatchObject({ kind: "post", event: "blocked" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), completed(2n), context())).toMatchObject({ kind: "post", event: "completed" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), agent({ attentionGeneration: 2n, attentionKind: "" }), context()))
      .toEqual({ kind: "skip", reason: "noEvent" });
  });

  it("step 3: without an attention advance, only a fresh blocked transition notifies", () => {
    // prev undefined (first sight) and blocked → blocked.
    expect(decideAgentNotification(undefined, blocked(1n), context())).toMatchObject({ kind: "post", event: "blocked" });
    // prev already blocked, same generation → nothing.
    expect(decideAgentNotification(blocked(1n), blocked(1n), context())).toEqual({ kind: "skip", reason: "noEvent" });
    // working → idle without attention advance → nothing.
    expect(decideAgentNotification(agent(), agent({ lifecycle: "idle" }), context())).toEqual({ kind: "skip", reason: "noEvent" });
    // A completed record seen for the first time is not an event without a prev to advance from.
    expect(decideAgentNotification(undefined, completed(3n), context())).toEqual({ kind: "skip", reason: "noEvent" });
  });

  it("step 4: at or below the notification watermark → nothing", () => {
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(5n), context({ notificationWatermark: 5n })))
      .toEqual({ kind: "skip", reason: "belowWatermark" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(6n), context({ notificationWatermark: 5n })))
      .toMatchObject({ kind: "post" });
  });

  it("step 5: the same (agentId, attentionGeneration) already notified → nothing", () => {
    const already = context({ alreadyNotified: (id, gen) => id === "a1" && gen === 2n });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), already)).toEqual({ kind: "skip", reason: "alreadyNotified" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(3n), already)).toMatchObject({ kind: "post" });
  });

  it("step 6: the focused pane in the foreground → nothing; background still posts", () => {
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), context({ focusedPaneId: "%1", appInForeground: true })))
      .toEqual({ kind: "skip", reason: "focused" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), context({ focusedPaneId: "%1", appInForeground: false })))
      .toMatchObject({ kind: "post" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), context({ focusedPaneId: "%2", appInForeground: true })))
      .toMatchObject({ kind: "post" });
  });

  it("step 6: a viewed agent is quiet without suppressing another agent", () => {
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), completed(2n), context({ viewedAgentId: "a1", appInForeground: true })))
      .toEqual({ kind: "skip", reason: "focused" });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), context({ viewedAgentId: "a2", appInForeground: true })))
      .toMatchObject({ kind: "post" });
  });

  it("step 7: exact title, body, tag and data", () => {
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), blocked(2n), context())).toEqual({
      kind: "post",
      event: "blocked",
      title: "muxflow · Fix tests",
      body: "Needs your input",
      tag: "a1",
      data: { agentId: "a1", paneId: "%1", sessionId: "$1", attentionGeneration: 2n },
    });
    expect(decideAgentNotification(agent({ attentionGeneration: 1n }), completed(2n), context())).toMatchObject({ body: "Finished" });
  });
});

import { describe, expect, it } from "vitest";
import type { Pane } from "./types";
import { notificationTopology, paneForResolvedNotification, resolveTerminalDestination } from "./paneRouting";
import { agentGeneration } from "../features/agents/generation";

const pane = (id: string, overrides: Partial<Pane> = {}): Pane => ({
  id,
  sessionId: "$1",
  windowId: "@1",
  index: 0,
  active: true,
  width: 80,
  height: 24,
  left: 0,
  top: 0,
  currentPath: "/tmp",
  currentCommand: "bash",
  ...overrides,
});

describe("authoritative pane routing", () => {
  const direct = pane("%1");
  const second = pane("%2");

  it("routes exact panes and never guesses missing pane IDs", () => {
    expect(resolveTerminalDestination([direct, second], "%1")).toEqual({ kind: "target", pane: direct });
    expect(resolveTerminalDestination([direct, second], "%2")).toEqual({ kind: "target", pane: second });
    expect(resolveTerminalDestination([direct], "%404").kind).toBe("unavailable");
  });

  it("maps only a canonical exact route onto a visible pane", () => {
    const snapshot = { sessions: [{ id: "$1", name: "one", windowCount: 1, attachedClients: 0 }], windows: [{ id: "@1", sessionId: "$1", index: 0, name: "shell", active: true, layout: "" }], panes: [direct, second] };
    expect(notificationTopology(snapshot, "server-a").targets[0]).toMatchObject({ sessionName: "one", paneIds: ["%1", "%2"] });
    expect(paneForResolvedNotification(snapshot, { resolution: "exact", sessionId: "$1", windowId: "@1", paneId: "%2", attentionGeneration: agentGeneration(1) })).toEqual({ kind: "target", pane: second });
    expect(paneForResolvedNotification(snapshot, { resolution: "expired" }).kind).toBe("unavailable");
    expect(paneForResolvedNotification(snapshot, { resolution: "wrongServer" }).kind).toBe("unavailable");
  });

  it("never substitutes a surviving pane for a closed or replaced target", () => {
    const snapshot = { sessions: [{ id: "$1", name: "one", windowCount: 1, attachedClients: 0 }], windows: [{ id: "@1", sessionId: "$1", index: 0, name: "shell", active: true, layout: "" }], panes: [direct] };
    expect(paneForResolvedNotification(snapshot, { resolution: "expired" }).kind).toBe("unavailable");
    expect(paneForResolvedNotification(snapshot, { resolution: "wrongServer" }).kind).toBe("unavailable");
  });
});

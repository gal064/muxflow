import { describe, expect, it, vi } from "vitest";
import {
  acknowledgeNotificationActivation, decideAgentNotification, emitNativeAgentNotification,
  emitTestNotification, notificationPermissionStatus,
} from "./notifications";
import { agent } from "./testFixtures";
import { agentGeneration } from "./generation";

const focus = { hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", windowId: "@1", paneId: "%1", appFocused: false, terminalVisible: true };

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

describe("agent native notification policy", () => {
  it("emits only background blocked and working-to-idle completion transitions", () => {
    const blocked = decideAgentNotification(agent(), agent({ lifecycle: "blocked", attentionGeneration: 4, lifecycleGeneration: 4 }), { focus, replayed: false });
    expect(blocked.kind).toBe("emit");
    const completed = decideAgentNotification(agent({ attentionGeneration: 4 }), agent({ lifecycle: "idle", attentionGeneration: 5, lifecycleGeneration: 4 }), { focus, replayed: false });
    expect(completed.kind).toBe("emit");
    expect(decideAgentNotification(undefined, agent({ lifecycle: "idle", attentionGeneration: 5 }), { focus, replayed: false }).kind).toBe("ignore");
  });

  it("suppresses focused, replayed, and duplicate generations", () => {
    const next = agent({ lifecycle: "blocked", attentionGeneration: 4, lifecycleGeneration: 4 });
    expect(decideAgentNotification(agent(), next, { focus: { ...focus, appFocused: true }, replayed: false })).toMatchObject({ kind: "suppress", instrumentation: { outcome: "suppressed-focused" } });
    expect(decideAgentNotification(agent(), next, { focus, replayed: true })).toMatchObject({ kind: "suppress", instrumentation: { outcome: "suppressed-replay" } });
    expect(decideAgentNotification(agent(), next, { focus, replayed: false, previouslyNotifiedGeneration: agentGeneration(4) })).toMatchObject({ kind: "suppress", instrumentation: { outcome: "suppressed-duplicate" } });
  });

  it("uses the persisted attention cause after a complete offline lifecycle cycle", () => {
    const before = agent({ lifecycle: "idle", attentionGeneration: 4, attentionKind: "completed" });
    const after = agent({ lifecycle: "idle", attentionGeneration: 6, attentionKind: "completed" });
    expect(decideAgentNotification(before, after, { focus, replayed: false, reconciledSnapshot: true }))
      .toMatchObject({ kind: "emit", notification: { event: "completed" } });
  });

  it("keeps prompt, output, path, and native hook identity out of lock-screen text", () => {
    const secret = agent({ displayName: "Codex\nAgent", lifecycle: "blocked", attentionGeneration: 4, nativeSessionId: "secret-native" });
    const decision = decideAgentNotification(agent(), secret, { focus, replayed: false, workspaceName: "work\nspace", windowName: "agent" });
    expect(decision.kind).toBe("emit");
    if (decision.kind !== "emit") return;
    expect(decision.notification.body).toBe("work space · agent · Blocked");
    expect(decision.notification.title).toBe("Codex Agent needs attention");
    expect(JSON.stringify(decision.notification)).not.toContain("secret-native");
  });

  it("names the terminal the way every other surface names it", () => {
    // The tab name still carries the CLI's own status ticker, which is a second
    // report of the state this notification exists to deliver — and no font on
    // the lock screen is guaranteed to have a glyph for it.
    const blocked = agent({ lifecycle: "blocked", attentionGeneration: 4 });
    const decision = decideAgentNotification(agent(), blocked, { focus, replayed: false, windowName: "✳ Fix tests" });
    expect(decision.kind).toBe("emit");
    if (decision.kind !== "emit") return;
    expect(decision.notification.body).toContain("· Fix tests ·");
  });

  it("shows unmapped attention explicitly without requesting or reporting a native action", async () => {
    const unmapped = agent({
      lifecycle: "blocked", attentionGeneration: 4, attentionKind: "blocked",
      sessionId: "", sessionName: "", windowId: "", windowName: "", paneId: "",
    });
    const decision = decideAgentNotification(agent(), unmapped, { focus, replayed: false });
    expect(decision).toMatchObject({
      kind: "emit",
      notification: { body: expect.stringContaining("Unmapped"), requestAction: false, route: { paneId: "" } },
    });
    if (decision.kind !== "emit") return;
    invokeMock.mockResolvedValueOnce({ id: 7, actionable: false });
    await expect(emitNativeAgentNotification(decision.notification)).resolves.toEqual({ id: 7, actionable: false });
    expect(invokeMock).toHaveBeenCalledWith("emit_agent_notification", expect.objectContaining({
      notification: expect.objectContaining({ requestAction: false, body: expect.stringContaining("Unmapped") }),
    }));
  });

  it("tells the OS to show what it decided the user cannot already see", async () => {
    // The delegate used to answer this itself, and answered "never while the
    // app is frontmost" — so a blocked agent in another workspace succeeded
    // invisibly. Focus is this side's fact, so this side sends the answer.
    const decision = decideAgentNotification(
      agent(),
      agent({ lifecycle: "blocked", attentionGeneration: 4, lifecycleGeneration: 4 }),
      { focus: { ...focus, appFocused: true, paneId: "%other" }, replayed: false },
    );
    expect(decision).toMatchObject({ kind: "emit", notification: { presentInForeground: true } });
    if (decision.kind !== "emit") return;
    invokeMock.mockResolvedValueOnce({ id: 3, actionable: true });
    await emitNativeAgentNotification(decision.notification);
    expect(invokeMock).toHaveBeenCalledWith("emit_agent_notification", expect.objectContaining({
      notification: expect.objectContaining({ presentInForeground: true }),
    }));
  });

  it("reads the permission and posts a test notification through their own commands", async () => {
    invokeMock.mockResolvedValueOnce("notDetermined");
    await expect(notificationPermissionStatus()).resolves.toBe("notDetermined");
    expect(invokeMock).toHaveBeenLastCalledWith("notification_permission_status");
    invokeMock.mockResolvedValueOnce({ id: 1, actionable: false });
    await expect(emitTestNotification()).resolves.toEqual({ id: 1, actionable: false });
    expect(invokeMock).toHaveBeenLastCalledWith("emit_test_notification");
    // Three platform backends write that vocabulary independently. A word this
    // side has no sentence for would render an empty status line, so it is
    // checked on the way in rather than asserted.
    invokeMock.mockResolvedValueOnce("ephemeral");
    await expect(notificationPermissionStatus()).resolves.toBe("unsupported");
  });

  it("acknowledges only the clicked agent and stale generation, not a newer pane peer", async () => {
    const markSeen = vi.fn(async () => undefined);
    await acknowledgeNotificationActivation({ markSeen }, {
      clientId: "client", hostProfileId: "local", serverIdentity: "server-a", topologyGeneration: 3, connectionEpoch: 1,
    }, { agentId: "clicked", attentionGeneration: agentGeneration("18446744073709551614") });
    expect(markSeen).toHaveBeenCalledTimes(1);
    expect(markSeen).toHaveBeenCalledWith(expect.anything(), "clicked", "18446744073709551614");
  });
});

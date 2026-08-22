import { describe, expect, it, vi } from "vitest";
import type { HostScopeToken } from "../shell/hostScope";
import { requestReconciledTmuxAction } from "./actionReconciliation";

const scope = (generation: number): HostScopeToken => ({
  hostProfileId: "remote",
  connectionKey: "ssh:remote",
  connectionEpoch: 7,
  serverIdentity: "server-a",
  generation,
});

describe("requestReconciledTmuxAction", () => {
  it("retries an implicit non-destructive action with the newer authoritative generation", async () => {
    const request = vi.fn()
      .mockRejectedValueOnce(new Error("stale topology: generation changed"))
      .mockResolvedValueOnce({ topologyGeneration: 4 });
    const waitForNewerScope = vi.fn(async () => scope(4));

    await requestReconciledTmuxAction({
      clientId: "client",
      action: { kind: "renameWindow", windowId: "@1", name: "Build" },
      initialScope: scope(3),
      currentScope: () => scope(4),
      request,
      waitForNewerScope,
    });

    expect(request).toHaveBeenCalledTimes(2);
    expect(request.mock.calls[0][2]).toEqual({ serverIdentity: "server-a", generation: 3 });
    expect(request.mock.calls[1][2]).toEqual({ serverIdentity: "server-a", generation: 4 });
    expect(waitForNewerScope).toHaveBeenCalledOnce();
  });

  it("never retries an action whose caller pinned a generation", async () => {
    for (const options of [
      { action: { kind: "renameSession", sessionId: "$1", name: "Work" } as const, capturedPrecondition: { serverIdentity: "server-a", generation: 2 } },
      // A confirmed close carries the generation the dialog showed the person.
      // Re-issuing against a newer one acts on a tmux they never saw.
      { action: { kind: "closeWindow", windowId: "@1", confirmed: true } as const, capturedPrecondition: { serverIdentity: "server-a", generation: 3 } },
    ]) {
      const request = vi.fn().mockRejectedValue(new Error("stale topology: generation changed"));
      const waitForNewerScope = vi.fn(async () => scope(4));
      await expect(requestReconciledTmuxAction({
        clientId: "client",
        initialScope: scope(3),
        currentScope: () => scope(4),
        request,
        waitForNewerScope,
        ...options,
      })).rejects.toThrow("stale topology");
      expect(request).toHaveBeenCalledOnce();
      expect(waitForNewerScope).not.toHaveBeenCalled();
    }
  });

  /**
   * A bulk close is a run of destructive actions against one server, and each
   * one moves the topology the next would have been measured against. The
   * refusal is raised before tmux is touched — the host's `stale topology`
   * bails all sit above the mutation — so re-issuing cannot close a second
   * window; refusing to re-issue is what left survivors behind.
   */
  it("retries a destructive close that guards only the server identity", async () => {
    const request = vi.fn()
      .mockRejectedValueOnce(new Error("stale topology: external tmux structural mutation was reconciled before action"))
      .mockResolvedValueOnce({ topologyGeneration: 6 });
    const waitedFrom: HostScopeToken[] = [];
    const waitForNewerScope = vi.fn(async (attempted: HostScopeToken) => {
      waitedFrom.push(attempted);
      return scope(6);
    });

    await requestReconciledTmuxAction({
      clientId: "client",
      action: { kind: "closeWindow", sessionId: "$1", windowId: "@2", confirmed: true },
      capturedPrecondition: { serverIdentity: "server-a", generation: 0 },
      initialScope: scope(5),
      currentScope: () => scope(6),
      request,
      waitForNewerScope,
    });

    expect(request).toHaveBeenCalledTimes(2);
    // Still guarding nothing but the server: a retry must not acquire the
    // generation guard the caller deliberately did not ask for.
    expect(request.mock.calls[0][2]).toEqual({ serverIdentity: "server-a", generation: 0 });
    expect(request.mock.calls[1][2]).toEqual({ serverIdentity: "server-a", generation: 0 });
    // Waited on the generation that was live when the refusal came, not on the
    // stamped 0, which every generation is already newer than.
    expect(waitedFrom[0]).toMatchObject({ generation: 5 });
  });

  it("bounds reconciliation to two retries", async () => {
    const request = vi.fn().mockRejectedValue(new Error("stale topology: generation changed"));
    let generation = 3;
    await expect(requestReconciledTmuxAction({
      clientId: "client",
      action: { kind: "renameWindow", windowId: "@1", name: "Build" },
      initialScope: scope(generation),
      currentScope: () => scope(generation),
      request,
      waitForNewerScope: vi.fn(async () => scope(++generation)),
    })).rejects.toThrow("stale topology");
    expect(request).toHaveBeenCalledTimes(3);
  });

  it.each([
    { hostProfileId: "replacement" },
    { connectionKey: "ssh:replacement" },
    { connectionEpoch: 8 },
    { serverIdentity: "server-b" },
  ])("rejects a successful completion from a replaced scope %o", async (replacement) => {
    await expect(requestReconciledTmuxAction({
      clientId: "client",
      action: { kind: "selectSession", sessionId: "$1" },
      initialScope: scope(3),
      currentScope: () => ({ ...scope(4), ...replacement }),
      request: vi.fn(async () => ({ topologyGeneration: 4 })),
    })).rejects.toThrow("connection changed");
  });
});

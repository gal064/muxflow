// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { HostScopeToken } from "../features/shell/hostScope";
import { recordIncident } from "../diagnostics/incidents";
import { useTmuxActionPerformer } from "./useTmuxActionPerformer";

vi.mock("../diagnostics/incidents", () => ({ recordIncident: vi.fn() }));
const incidents = vi.mocked(recordIncident);

const scope: HostScopeToken = {
  hostProfileId: "remote", connectionKey: "ssh:remote", connectionEpoch: 1,
  serverIdentity: "server-a", generation: 1,
};

async function performer(requestAction: NonNullable<Parameters<typeof useTmuxActionPerformer>[0]["requestAction"]>) {
  const setStatus = vi.fn();
  let perform!: ReturnType<typeof useTmuxActionPerformer>;
  function Harness() {
    perform = useTmuxActionPerformer({
      canMutate: true, clientId: "client", generation: 1,
      hostScopeRef: { current: scope }, requestAction, serverIdentity: "server-a", setStatus,
    });
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness />); });
  return { perform, renderer, setStatus };
}

describe("tmux action execution feedback boundary", () => {
  it("keeps both successful and failed internal pane navigation silent", async () => {
    const requestAction = vi.fn().mockResolvedValueOnce({ topologyGeneration: 2 }).mockRejectedValueOnce(new Error("stale"));
    const harness = await performer(requestAction);
    await act(async () => {
      await harness.perform({ kind: "focusPane", sessionId: "$1", windowId: "@1", paneId: "%1" }, undefined, {
        kind: "navigation", feedback: "silent", measurePanePaint: false,
      });
      await expect(harness.perform(
        { kind: "focusPane", sessionId: "$1", windowId: "@1", paneId: "%2" },
        undefined,
        { kind: "navigation", feedback: "silent", measurePanePaint: false },
      )).rejects.toThrow("stale");
    });
    expect(harness.setStatus).not.toHaveBeenCalled();
    await act(async () => harness.renderer.unmount());
  });

  it("retains routine progress reporting for ordinary user-visible actions", async () => {
    const harness = await performer(vi.fn(async () => ({ topologyGeneration: 2 })));
    await act(async () => { await harness.perform({ kind: "selectWindow", sessionId: "$1", windowId: "@1" }); });
    expect(harness.setStatus).toHaveBeenCalledWith("Waiting for authoritative tmux state…");
    await act(async () => harness.renderer.unmount());
  });

  it("reports and preserves failures for visible navigation", async () => {
    const stale = new Error("stale topology generation");
    const harness = await performer(vi.fn(async () => { throw stale; }));
    await act(async () => {
      await expect(harness.perform(
        { kind: "selectWindow", sessionId: "$1", windowId: "@1" },
        undefined,
        { kind: "navigation", feedback: "visible", measurePanePaint: true },
      )).rejects.toBe(stale);
    });
    expect(harness.setStatus).toHaveBeenCalledWith(String(stale));
    await act(async () => harness.renderer.unmount());
  });

  /**
   * A refusal that only ever became a status string was a refusal nobody could
   * investigate: the next action overwrites the message, and a bulk close
   * overwrote its own with the following tab's progress. The journal is where
   * "the host said no, and this is what it said" survives the render after.
   */
  it("journals every refusal it turns into a status message", async () => {
    incidents.mockClear();
    const harness = await performer(vi.fn(async () => { throw new Error("stale topology: generation changed"); }));
    await act(async () => {
      await harness.perform({ kind: "closeWindow", sessionId: "$1", windowId: "@1", confirmed: true });
    });
    expect(incidents).toHaveBeenCalledWith("action.refused", {
      kind: "closeWindow",
      error: expect.stringContaining("stale topology: generation changed"),
    });
    // Bounded: an error carrying a whole tmux transcript must not become the
    // journal's largest line.
    const [, detail] = incidents.mock.calls[0] as [string, { error: string }];
    expect(detail.error.length).toBeLessThanOrEqual(200);
    await act(async () => harness.renderer.unmount());
  });

  it("retries stale standalone pane focus because no captured precondition is supplied", async () => {
    const scopeRef = { current: scope };
    const request = vi.fn()
      .mockRejectedValueOnce(new Error("stale topology: generation changed"))
      .mockResolvedValueOnce({ topologyGeneration: 2 });
    const waitForNewerScope = vi.fn(async () => {
      scopeRef.current = { ...scope, generation: 2 };
      return scopeRef.current;
    });
    let perform!: ReturnType<typeof useTmuxActionPerformer>;
    function Harness() {
      perform = useTmuxActionPerformer({
        canMutate: true, clientId: "client", generation: 1, hostScopeRef: scopeRef,
        reconciliation: { request, waitForNewerScope }, serverIdentity: "server-a", setStatus: vi.fn(),
      });
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    await act(async () => { await perform({ kind: "focusPane", sessionId: "$1", windowId: "@1", paneId: "%1" }); });
    expect(request).toHaveBeenCalledTimes(2);
    expect(request.mock.calls.map((call) => call[2].generation)).toEqual([1, 2]);
    await act(async () => renderer.unmount());
  });
});

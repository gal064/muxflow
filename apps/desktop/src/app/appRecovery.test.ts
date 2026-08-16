import { describe, expect, it } from "vitest";
import type { HostScopeToken } from "../features/shell/hostScope";
import { appRecoveryDiscardState, appRecoveryReducer, type AppRecoveryState } from "./appRecovery";

const scope = {
  hostProfileId: "remote",
  connectionKey: "ssh:remote",
  connectionEpoch: 4,
  serverIdentity: "server-a",
  generation: 8,
} satisfies HostScopeToken & { serverIdentity: string };

const offer = (): AppRecoveryState => appRecoveryReducer(undefined, {
  type: "offer",
  count: 2,
  previousServerIdentity: "server-old",
  scope,
})!;

describe("app tab recovery state", () => {
  it("keeps an offered or confirming recovery through topology-only generation changes", () => {
    const offered = offer();
    expect(appRecoveryReducer(offered, { type: "reconcileScope", scope: { ...scope, generation: 9 } })).toBe(offered);
    const confirming = appRecoveryReducer(offered, { type: "confirmDiscard" });
    expect(appRecoveryReducer(confirming, { type: "reconcileScope", scope: { ...scope, generation: 10 } })).toBe(confirming);
    expect(appRecoveryDiscardState(confirming)).toBe(confirming);
  });

  it.each([
    { hostProfileId: "other" },
    { connectionKey: "ssh:replacement" },
    { connectionEpoch: 5 },
    { serverIdentity: "server-b" },
  ])("clears the whole confirmation on durable scope replacement %o", (replacement) => {
    const confirming = appRecoveryReducer(offer(), { type: "confirmDiscard" });
    const reconciled = appRecoveryReducer(confirming, {
      type: "reconcileScope",
      scope: { ...scope, ...replacement },
    });
    expect(reconciled).toBeUndefined();
    expect(appRecoveryDiscardState(reconciled)).toBeUndefined();
  });
});

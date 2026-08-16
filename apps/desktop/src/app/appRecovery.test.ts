import { describe, expect, it } from "vitest";
import type { HostScopeToken } from "../features/shell/hostScope";
import {
  appRecoveryModalOpen,
  confirmAppRecoveryDiscard,
  offerAppRecovery,
  reconcileAppRecovery,
} from "./appRecovery";

const scope: HostScopeToken = {
  hostProfileId: "remote",
  connectionKey: "ssh:remote",
  connectionEpoch: 4,
  serverIdentity: "server-a",
  generation: 8,
};

const offer = () => offerAppRecovery({
  hostProfileId: "remote",
  previousServerIdentity: "server-old",
  currentServerIdentity: "server-a",
  count: 2,
  scope,
});

describe("app tab recovery state", () => {
  it("keeps an offered or confirming recovery through topology-only generation changes", () => {
    const offered = offer();
    expect(reconcileAppRecovery(offered, { ...scope, generation: 9 })).toBe(offered);
    const confirming = confirmAppRecoveryDiscard(offered);
    expect(reconcileAppRecovery(confirming, { ...scope, generation: 10 })).toBe(confirming);
    expect(appRecoveryModalOpen(confirming)).toBe(true);
  });

  it.each([
    { hostProfileId: "other" },
    { connectionKey: "ssh:replacement" },
    { connectionEpoch: 5 },
    { serverIdentity: "server-b" },
  ])("clears the whole confirmation on durable scope replacement %o", (replacement) => {
    const reconciled = reconcileAppRecovery(confirmAppRecoveryDiscard(offer()), { ...scope, ...replacement });
    expect(reconciled).toBeUndefined();
    expect(appRecoveryModalOpen(reconciled)).toBe(false);
  });
});

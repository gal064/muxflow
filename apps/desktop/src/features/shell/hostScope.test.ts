import { describe, expect, it } from "vitest";
import { sameHostConnection, sameHostScope, type HostScopeToken } from "./hostScope";

const token: HostScopeToken = { hostProfileId: "local", connectionKey: "local", connectionEpoch: 1, serverIdentity: "server", generation: 2 };

describe("host scope token", () => {
  it("invalidates every deferred operation dimension", () => {
    expect(sameHostScope(token, { ...token })).toBe(true);
    for (const changed of [
      { hostProfileId: "other" }, { connectionKey: "other" }, { connectionEpoch: 2 },
      { serverIdentity: "other" }, { generation: 3 },
    ]) expect(sameHostScope(token, { ...token, ...changed })).toBe(false);
  });

  it("keeps a dialog valid across topology generations on the same connection", () => {
    expect(sameHostConnection(token, { ...token, generation: 3 })).toBe(true);
  });

  it("invalidates a dialog when the durable connection identity changes", () => {
    for (const changed of [
      { hostProfileId: "other" }, { connectionKey: "ssh:other" }, { connectionEpoch: 2 },
      { serverIdentity: "other" },
    ]) expect(sameHostConnection(token, { ...token, ...changed })).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import {
  helperOwnsHostSetupLane, helperUpgradeReducer, initialHelperUpgradeState, type RemoteHelperProbe,
} from "./helperUpgrade";

const probe: RemoteHelperProbe = {
  operatingSystem: "Linux",
  architecture: "x86_64",
  tmuxVersion: "tmux 3.3a",
  gitVersion: "git version 2.39",
  installed: true,
  helperVersion: "0.0.1",
  compatible: false,
  remotePath: "$HOME/.local/bin/muxflow-host",
};

describe("helper upgrade experience", () => {
  it("requires a probe and explicit confirmation before upgrade", () => {
    const probing = helperUpgradeReducer(initialHelperUpgradeState, { type: "probe", connectionKey: "host-a" });
    const ready = helperUpgradeReducer(probing, { type: "probeSucceeded", connectionKey: "host-a", probe });
    const confirming = helperUpgradeReducer(ready, { type: "requestUpgrade" });
    expect(confirming.phase).toBe("confirming");
    expect(helperUpgradeReducer(confirming, { type: "upgrade" }).phase).toBe("upgrading");
  });

  it("distinguishes successful rollback from rollback failure and remains retryable", () => {
    const upgrading = { phase: "upgrading" as const, connectionKey: "host-a", operation: "upgrade" as const, probe };
    const restored = helperUpgradeReducer(upgrading, { type: "upgradeFailed", connectionKey: "host-a", message: "handshake failed", rollback: "restored" });
    const failed = helperUpgradeReducer(upgrading, { type: "upgradeFailed", connectionKey: "host-a", message: "disk full", rollback: "failed" });
    expect(restored).toMatchObject({ phase: "failed", rollback: "restored", probe });
    expect(failed).toMatchObject({ phase: "failed", rollback: "failed", probe });
    expect(helperUpgradeReducer(restored, { type: "probe", connectionKey: "host-a" }).phase).toBe("probing");
    expect(helperUpgradeReducer(restored, { type: "abandonProbe", connectionKey: "host-a" })).toEqual(restored);
  });

  it("abandons only the probe for the connection that actually dropped", () => {
    const probing = helperUpgradeReducer(initialHelperUpgradeState, { type: "probe", connectionKey: "host-a" });
    expect(helperUpgradeReducer(probing, { type: "abandonProbe", connectionKey: "host-b" })).toEqual(probing);
    expect(helperUpgradeReducer(probing, { type: "abandonProbe", connectionKey: "host-a" })).toEqual(initialHelperUpgradeState);
  });

  it("invalidates a settled connection probe without interrupting an upgrade", () => {
    const probing = helperUpgradeReducer(initialHelperUpgradeState, { type: "probe", connectionKey: "host-a" });
    const ready = helperUpgradeReducer(probing, { type: "probeSucceeded", connectionKey: "host-a", probe });
    const confirming = helperUpgradeReducer(ready, { type: "requestUpgrade" });
    const upgrading = helperUpgradeReducer(confirming, { type: "upgrade" });

    expect(helperUpgradeReducer(confirming, { type: "invalidateConnectionProbe", connectionKey: "host-a" }))
      .toEqual(initialHelperUpgradeState);
    expect(helperUpgradeReducer(upgrading, { type: "invalidateConnectionProbe", connectionKey: "host-a" }))
      .toEqual(upgrading);
    expect(helperUpgradeReducer(upgrading, { type: "probe", connectionKey: "host-a" }))
      .toEqual(upgrading);
  });

  it("discards stale helper completions after a host change", () => {
    const hostA = helperUpgradeReducer(initialHelperUpgradeState, { type: "probe", connectionKey: "host-a" });
    const hostB = helperUpgradeReducer(hostA, { type: "probe", connectionKey: "host-b" });
    expect(helperUpgradeReducer(hostB, { type: "probeSucceeded", connectionKey: "host-a", probe })).toEqual(hostB);
  });

  it("owns the host setup lane only while checking, confirming, or replacing the helper", () => {
    const probing = helperUpgradeReducer(initialHelperUpgradeState, { type: "probe", connectionKey: "host-a" });
    const ready = helperUpgradeReducer(probing, { type: "probeSucceeded", connectionKey: "host-a", probe });
    const confirming = helperUpgradeReducer(ready, { type: "requestUpgrade" });
    const upgrading = helperUpgradeReducer(confirming, { type: "upgrade" });
    const failed = helperUpgradeReducer(upgrading, {
      type: "upgradeFailed", connectionKey: "host-a", message: "no", rollback: "restored",
    });

    expect(helperOwnsHostSetupLane(initialHelperUpgradeState)).toBe(false);
    expect(helperOwnsHostSetupLane(probing)).toBe(true);
    expect(helperOwnsHostSetupLane(confirming)).toBe(true);
    expect(helperOwnsHostSetupLane(upgrading)).toBe(true);
    expect(helperOwnsHostSetupLane(ready)).toBe(false);
    expect(helperOwnsHostSetupLane(failed)).toBe(false);
  });
});

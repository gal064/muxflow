import { describe, expect, it } from "vitest";
import { helperUpgradeReducer, initialHelperUpgradeState, type RemoteHelperProbe } from "./helperUpgrade";

const probe: RemoteHelperProbe = {
  operatingSystem: "Linux",
  architecture: "x86_64",
  tmuxVersion: "tmux 3.3a",
  gitVersion: "git version 2.39",
  installed: true,
  helperVersion: "0.0.1",
  compatible: false,
  remotePath: "$HOME/.local/bin/tmux-ide-host",
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
  });

  it("discards stale helper completions after a host change", () => {
    const hostA = helperUpgradeReducer(initialHelperUpgradeState, { type: "probe", connectionKey: "host-a" });
    const hostB = helperUpgradeReducer(hostA, { type: "probe", connectionKey: "host-b" });
    expect(helperUpgradeReducer(hostB, { type: "probeSucceeded", connectionKey: "host-a", probe })).toEqual(hostB);
  });
});

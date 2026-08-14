import { describe, expect, it } from "vitest";
import { hookWiringNotice, hostHookWiring, setupAdapterIds } from "./hookWiring";
import type { AgentAdapterDescriptor, AgentHookWiring } from "./types";

const adapter = (id: string, hookWiring: AgentHookWiring, overrides: Partial<AgentAdapterDescriptor> = {}): AgentAdapterDescriptor => ({
  id, displayName: id, supportsLaunch: true, supportsResume: true, supportsHooks: true,
  supportsProcessDetection: true, supportsScreenFallback: true,
  hookConfigPath: `/home/user/.${id}/settings.json`, hookEvents: [], placements: ["window", "split"],
  hookWiring, hookWiringDetail: "", ...overrides,
});

describe("what this host is allowed to say about agent status", () => {
  it("says nothing is reported when every adapter's events go somewhere else", () => {
    // The field configuration: hooks exist for every event and all of them
    // belong to another tool, so the daemon has never heard from an agent.
    const wiring = hostHookWiring([adapter("claude-code", "notWired"), adapter("codex", "notWired")]);
    expect(wiring.reports).toBe(false);
    expect(setupAdapterIds(wiring)).toEqual(["claude-code", "codex"]);
    expect(hookWiringNotice(wiring)).toBe("Agent status unavailable on this host — set up hooks");
  });

  it("treats a partial install as something to finish, not as wired", () => {
    // An install that covers some events leaves transitions that can never
    // arrive — an agent that starts and never finishes, for instance.
    const wiring = hostHookWiring([adapter("claude-code", "partial")]);
    expect(wiring.reports).toBe(false);
    expect(setupAdapterIds(wiring)).toEqual(["claude-code"]);
  });

  it("stays quiet once any adapter reports, and offers to finish the other", () => {
    const wiring = hostHookWiring([adapter("claude-code", "wired"), adapter("codex", "notWired")]);
    expect(wiring.reports).toBe(true);
    expect(hookWiringNotice(wiring)).toBeUndefined();
    expect(setupAdapterIds(wiring)).toEqual(["codex"]);
  });

  it("never offers to write over a configuration it could not read", () => {
    const wiring = hostHookWiring([adapter("claude-code", "unavailable", { hookWiringDetail: "parse hook JSON configuration" })]);
    expect(wiring.setupTargets).toEqual([]);
    expect(wiring.unreadable.map((item) => item.id)).toEqual(["claude-code"]);
    expect(hookWiringNotice(wiring))
      .toBe("Agent status unavailable on this host — its agent configuration could not be read");
  });

  it("says nothing at all before the host has answered", () => {
    // Every render before the first snapshot, and every render against a host
    // too old to have an opinion. Neither is evidence that hooks are missing.
    for (const adapters of [[], [adapter("claude-code", "unspecified")]]) {
      const wiring = hostHookWiring(adapters);
      expect(wiring.unknown).toBe(true);
      expect(wiring.reports).toBe(false);
      expect(hookWiringNotice(wiring)).toBeUndefined();
    }
  });

  it("ignores adapters that have no hooks to wire", () => {
    const wiring = hostHookWiring([adapter("screen-only", "notWired", { supportsHooks: false })]);
    expect(wiring.unknown).toBe(true);
    expect(wiring.setupTargets).toEqual([]);
  });
});

import { describe, expect, it } from "vitest";
import { hookWiringNotice, hostHookWiring, shouldPromptForSetup } from "./hookWiring";
import type { AgentAdapterDescriptor, AgentHookWiring } from "./types";

const adapter = (id: string, hookWiring: AgentHookWiring, overrides: Partial<AgentAdapterDescriptor> = {}): AgentAdapterDescriptor => ({
  id, displayName: id, supportsLaunch: true, supportsResume: true, supportsHooks: true,
  supportsProcessDetection: true, supportsScreenFallback: true,
  hookConfigPath: `/home/user/.${id}/settings.json`, hookEvents: [], placements: ["window", "split"],
  hookWiring, hookWiringDetail: "", hookSetupRecommended: hookWiring === "notWired" || hookWiring === "partial",
  ...overrides,
});

describe("what this host is allowed to say about agent status", () => {
  it("says nothing is reported when every adapter's events go somewhere else", () => {
    // The field configuration: hooks exist for every event and all of them
    // belong to another tool, so the daemon has never heard from an agent.
    const wiring = hostHookWiring([adapter("claude-code", "notWired"), adapter("codex", "notWired")]);
    expect(wiring.reports).toBe(false);
    expect(wiring.setupTargets.map((item) => item.id)).toEqual(["claude-code", "codex"]);
    expect(hookWiringNotice(wiring)).toBe("Agent status unavailable on this host — set up hooks");
  });

  it("treats a partial install as something to finish, not as wired", () => {
    // An install that covers some events leaves transitions that can never
    // arrive — an agent that starts and never finishes, for instance.
    const wiring = hostHookWiring([adapter("claude-code", "partial")]);
    expect(wiring.reports).toBe(false);
    expect(wiring.setupTargets.map((item) => item.id)).toEqual(["claude-code"]);
  });

  it("stays quiet once any adapter reports, and offers to finish the other", () => {
    const wiring = hostHookWiring([adapter("claude-code", "wired"), adapter("codex", "notWired")]);
    expect(wiring.reports).toBe(true);
    expect(hookWiringNotice(wiring)).toBeUndefined();
    expect(wiring.setupTargets.map((item) => item.id)).toEqual(["codex"]);
  });

  it("never offers to write over a configuration it could not read", () => {
    const wiring = hostHookWiring([adapter("claude-code", "unavailable", { hookWiringDetail: "parse hook JSON configuration" })]);
    expect(wiring.setupTargets).toEqual([]);
    expect(wiring.unreadableReason).toBe("parse hook JSON configuration");
    expect(hookWiringNotice(wiring))
      .toBe("Agent status unavailable on this host — parse hook JSON configuration");
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

  it("never offers to set up an agent that is not installed on the host", () => {
    // Absent looks exactly like unwired from a missing config file. Treating
    // the two the same meant accepting the prompt created `~/.codex/hooks.json`
    // on a machine that has never had Codex — configuration for a tool the
    // user does not use, written on their behalf.
    const wiring = hostHookWiring([adapter("claude-code", "wired"), adapter("codex", "absent")]);
    expect(wiring.setupTargets).toEqual([]);
    expect(wiring.reports).toBe(true);
    expect(shouldPromptForSetup(wiring)).toBe(false);

    const nothingHere = hostHookWiring([adapter("claude-code", "absent"), adapter("codex", "absent")]);
    expect(nothingHere.unknown).toBe(true);
    expect(hookWiringNotice(nothingHere)).toBeUndefined();
  });

  it("only raises the prompt by itself when the whole host reports nothing", () => {
    // The gap is still offered from Settings and from the section's own line;
    // what it does not do is interrupt someone whose status already works.
    const partlyWired = hostHookWiring([adapter("claude-code", "wired"), adapter("codex", "notWired")]);
    expect(shouldPromptForSetup(partlyWired)).toBe(false);
    expect(partlyWired.setupTargets.map((item) => item.id)).toEqual(["codex"]);

    const silent = hostHookWiring([adapter("claude-code", "notWired"), adapter("codex", "notWired")]);
    expect(shouldPromptForSetup(silent)).toBe(true);

    // And never before the host has said anything at all.
    expect(shouldPromptForSetup(hostHookWiring([]))).toBe(false);
    expect(shouldPromptForSetup(hostHookWiring([adapter("claude-code", "unspecified")]))).toBe(false);
  });
});

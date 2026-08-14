// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAgentHostSetup, type AgentHostSetupOptions } from "./useAgentHostSetup";
import type { AgentAdapterDescriptor, AgentHookReview, AgentHookWiring } from "./types";

const adapter = (id: string, hookWiring: AgentHookWiring): AgentAdapterDescriptor => ({
  id, displayName: id, supportsLaunch: true, supportsResume: true, supportsHooks: true,
  supportsProcessDetection: true, supportsScreenFallback: true,
  hookConfigPath: `/home/user/.${id}/settings.json`, hookEvents: [], placements: ["window", "split"],
  hookWiring, hookWiringDetail: "",
});

const review = (adapterId: string, alreadyInstalled = false): AgentHookReview => ({
  adapterId, action: "install", revision: `token-${adapterId}`, alreadyInstalled,
  changes: [], managedLabel: "3",
});

function harness(overrides: Partial<AgentHostSetupOptions> = {}) {
  const calls = {
    reviewHooks: vi.fn(async (id: string) => review(id)),
    applyHooks: vi.fn(async () => undefined),
    recordDecision: vi.fn(),
    refreshWiring: vi.fn(),
    onStatus: vi.fn(),
    onModalChange: vi.fn(),
    openReview: vi.fn(),
    applyHostNaming: vi.fn(async () => "applied" as const),
  };
  const options: AgentHostSetupOptions = {
    adapters: [adapter("claude-code", "notWired")],
    connected: true,
    connectionKey: "ssh-omarchy\0server-a\u00001",
    hostProfileId: "ssh-omarchy",
    hostLabel: "omarchy",
    decision: undefined,
    ...calls,
    ...overrides,
  };
  let setup!: ReturnType<typeof useAgentHostSetup>;
  function Harness(props: Partial<AgentHostSetupOptions>) {
    setup = useAgentHostSetup({ ...options, ...props });
    return <div>{setup.dialog}</div>;
  }
  return { calls, options, Harness, get current() { return setup; } };
}

describe("the one-time set-up prompt", () => {
  beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); });

  it("asks once when a connected host cannot report status", async () => {
    const { calls, Harness } = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    const html = JSON.stringify(renderer.toJSON());
    expect(html).toContain("Set up agent status on ");
    expect(html).toContain("omarchy");
    expect(html).toContain("One-time setup");
    // The exact files it would change are named: that is the part a user would
    // say no to, so it is not hidden behind the review.
    expect(html).toContain("/home/user/.claude-code/settings.json");
    expect(calls.onModalChange).toHaveBeenLastCalledWith(true);
    await act(async () => renderer.unmount());
  });

  it("never asks again once the host has an answer, of either kind", async () => {
    for (const decision of ["accepted", "declined"] as const) {
      const { calls, Harness } = harness({ decision });
      let renderer!: ReturnType<typeof create>;
      await act(async () => { renderer = create(<Harness />); });
      expect(renderer.toJSON()).toEqual({ type: "div", props: {}, children: null });
      expect(calls.onModalChange).not.toHaveBeenCalledWith(true);
      await act(async () => renderer.unmount());
    }
  });

  it("installs every adapter that needs it, records the answer, and re-reads the host", async () => {
    const { calls, Harness } = harness({ adapters: [adapter("claude-code", "notWired"), adapter("codex", "partial")] });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    const accept = renderer.root.findAll((node) => node.type === "button")
      .find((node) => String(node.children[0]).startsWith("Set up this host"))!;
    await act(async () => accept.props.onClick());
    expect(calls.reviewHooks.mock.calls.map(([id]) => id)).toEqual(["claude-code", "codex"]);
    expect(calls.applyHooks).toHaveBeenCalledTimes(2);
    expect(calls.recordDecision).toHaveBeenCalledWith("ssh-omarchy", "accepted");
    expect(calls.refreshWiring).toHaveBeenCalledTimes(1);
    await act(async () => renderer.unmount());
  });

  it("writes nothing when the host says the entries are already current", async () => {
    const { calls, Harness } = harness({ reviewHooks: vi.fn(async (id: string) => review(id, true)) });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    const accept = renderer.root.findAll((node) => node.type === "button")
      .find((node) => String(node.children[0]).startsWith("Set up this host"))!;
    await act(async () => accept.props.onClick());
    expect(calls.applyHooks).not.toHaveBeenCalled();
    expect(calls.recordDecision).toHaveBeenCalledWith("ssh-omarchy", "accepted");
    await act(async () => renderer.unmount());
  });

  it("records nothing when the install fails, and says what went wrong", async () => {
    // A failed attempt is not an answer. Recording it would mean the user is
    // never asked again about a host that was never set up.
    const { calls, Harness } = harness({ applyHooks: vi.fn(async () => { throw new Error("hook review is stale"); }) });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    const accept = renderer.root.findAll((node) => node.type === "button")
      .find((node) => String(node.children[0]).startsWith("Set up this host"))!;
    await act(async () => accept.props.onClick());
    expect(calls.recordDecision).not.toHaveBeenCalled();
    expect(JSON.stringify(renderer.toJSON())).toContain("hook review is stale");
    await act(async () => renderer.unmount());
  });

  it("remembers a decline, and still leaves a way back", async () => {
    const setup = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    const decline = renderer.root.findAll((node) => node.type === "button")
      .find((node) => node.children[0] === "Not now")!;
    await act(async () => decline.props.onClick());
    expect(setup.calls.recordDecision).toHaveBeenCalledWith("ssh-omarchy", "declined");
    expect(renderer.toJSON()).toEqual({ type: "div", props: {}, children: null });
    // Declining does not hide the door: Settings and the sidebar's own line
    // both drive `offer`, and the prompt comes back when they do.
    expect(setup.current.offerable).toBe(true);
    await act(async () => setup.current.offer());
    expect(JSON.stringify(renderer.toJSON())).toContain("Set up this host");
    await act(async () => renderer.unmount());
  });

  it("offers nothing, and shows no prompt, for a configuration it could not read", async () => {
    const setup = harness({ adapters: [adapter("claude-code", "unavailable")] });
    const { calls, Harness } = setup;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    expect(setup.current.offerable).toBe(false);
    expect(setup.current.notice)
      .toBe("Agent status unavailable on this host — its agent configuration could not be read");
    expect(calls.onModalChange).not.toHaveBeenCalledWith(true);
    await act(async () => renderer.unmount());
  });

  /**
   * Phase 13.5: the window naming lives in the tmux server's memory, so it is
   * asserted per connection rather than installed once — but only where the
   * user has already said yes, and never twice for the same connection.
   */
  it("re-asserts the tmux window naming once per connection, only after a yes", async () => {
    const setup = harness({ decision: undefined });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(setup.calls.applyHostNaming).not.toHaveBeenCalled();

    await act(async () => renderer.update(<setup.Harness decision="declined" />));
    expect(setup.calls.applyHostNaming).not.toHaveBeenCalled();

    await act(async () => renderer.update(<setup.Harness decision="accepted" />));
    expect(setup.calls.applyHostNaming).toHaveBeenCalledTimes(1);
    // A re-render on the same connection is not a new tmux server.
    await act(async () => renderer.update(<setup.Harness decision="accepted" />));
    expect(setup.calls.applyHostNaming).toHaveBeenCalledTimes(1);
    // A replaced server is, and it dropped the in-memory hook with it.
    await act(async () => renderer.update(<setup.Harness connectionKey="ssh-omarchy\0server-b 2" decision="accepted" />));
    expect(setup.calls.applyHostNaming).toHaveBeenCalledTimes(2);
    await act(async () => renderer.unmount());
  });

  it("keeps the hooks when the tmux naming is refused, and says so", async () => {
    // The naming is explicitly non-gating: agent status works without it.
    const setup = harness({ applyHostNaming: vi.fn(async () => { throw new Error("tmux rejected the recommended window naming"); }) });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    const accept = renderer.root.findAll((node) => node.type === "button")
      .find((node) => String(node.children[0]).startsWith("Set up this host"))!;
    await act(async () => accept.props.onClick());
    expect(setup.calls.recordDecision).toHaveBeenCalledWith("ssh-omarchy", "accepted");
    expect(setup.calls.onStatus).toHaveBeenCalledWith(expect.stringContaining("window naming was not applied"));
    await act(async () => renderer.unmount());
  });

  it("takes its question away with the host it was about", async () => {
    const { calls, Harness } = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    await act(async () => renderer.update(<Harness connected={false} />));
    expect(renderer.toJSON()).toEqual({ type: "div", props: {}, children: null });
    expect(calls.onModalChange).toHaveBeenLastCalledWith(false);
    await act(async () => renderer.unmount());
  });
});

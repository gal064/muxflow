// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAgentHostSetup, type AgentHostSetupOptions } from "./useAgentHostSetup";
import type { AgentAdapterDescriptor, AgentHookReview, AgentHookWiring } from "./types";

const adapter = (id: string, hookWiring: AgentHookWiring): AgentAdapterDescriptor => ({
  id, displayName: id, supportsLaunch: true, supportsResume: true, supportsHooks: true,
  supportsProcessDetection: true, supportsScreenFallback: true,
  hookConfigPath: `/home/user/.${id}/settings.json`, hookEvents: [], placements: ["window", "split"],
  hookWiring, hookWiringDetail: "", hookSetupRecommended: hookWiring === "notWired" || hookWiring === "partial",
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
    openReview: vi.fn(),
    applyHostNaming: vi.fn(async () => "applied" as const),
  };
  const options: AgentHostSetupOptions = {
    adapters: [adapter("claude-code", "notWired")],
    connected: true,
    hostProfileId: "ssh-omarchy",
    hostIdentity: "ssh-omarchy client-1 1",
    decisionsArePersistable: true,
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
    const setup = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    const html = JSON.stringify(renderer.toJSON());
    expect(html).toContain("Set up agent status on ");
    expect(html).toContain("omarchy");
    expect(html).toContain("One-time setup");
    // The exact files it would change are named: that is the part a user would
    // say no to, so it is not hidden behind the review.
    expect(html).toContain("/home/user/.claude-code/settings.json");
    expect(setup.current.open).toBe(true);
    await act(async () => renderer.unmount());
  });

  it("never asks again once the host has an answer, of either kind", async () => {
    for (const decision of ["accepted", "declined"] as const) {
      const setup = harness({ decision });
      let renderer!: ReturnType<typeof create>;
      await act(async () => { renderer = create(<setup.Harness />); });
      expect(renderer.toJSON()).toEqual({ type: "div", props: {}, children: null });
      expect(setup.current.open).toBe(false);
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
    const { Harness } = setup;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    expect(setup.current.offerable).toBe(false);
    // No reason invented: the fixture gave no detail, so the line stops.
    expect(setup.current.notice).toBe("Agent status unavailable on this host");
    expect(setup.current.open).toBe(false);
    await act(async () => renderer.unmount());
  });

  /**
   * Phase 13.5: the window naming lives in the tmux server's memory, so it is
   * asserted rather than installed — but only where the user has already said
   * yes. Repetition is the host's problem: `applyHostNaming` is idempotent and
   * answers `alreadyCurrent` without writing.
   */
  it("asserts the tmux window naming only after a yes", async () => {
    const setup = harness({ decision: undefined });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(setup.calls.applyHostNaming).not.toHaveBeenCalled();

    await act(async () => renderer.update(<setup.Harness decision="declined" />));
    expect(setup.calls.applyHostNaming).not.toHaveBeenCalled();

    await act(async () => renderer.update(<setup.Harness decision="accepted" />));
    expect(setup.calls.applyHostNaming).toHaveBeenCalledTimes(1);
    // Exactly once per pass: the install path asserts it itself, so an
    // already-current host and a just-installed one both get one call.
    await act(async () => renderer.update(<setup.Harness decision="accepted" />));
    expect(setup.calls.applyHostNaming).toHaveBeenCalledTimes(1);
    // A reconnect is a new tmux server as far as this is concerned, and a
    // restarted one dropped the in-memory hook with it.
    await act(async () => renderer.update(<setup.Harness connected={false} decision="accepted" />));
    await act(async () => renderer.update(<setup.Harness decision="accepted" />));
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

  /**
   * The consent is to keeping this host set up, not to one particular set of
   * hook events. When the managed set grows — which it did this phase, 2 to 3 —
   * an already-consented host reads as partially wired; without this it would
   * say "agent status unavailable" forever, with the one-time prompt already
   * answered and unable to come back and fix it.
   */
  it("brings an already-consented host up to date without asking again", async () => {
    // `partial` is the shape a grown event set leaves: this app already owns
    // entries in that file, and some of the events it now needs are missing.
    const setup = harness({ decision: "accepted", adapters: [adapter("claude-code", "partial")] });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(renderer.toJSON()).toEqual({ type: "div", props: {}, children: null });
    expect(setup.calls.applyHooks).toHaveBeenCalledTimes(1);
    expect(setup.calls.refreshWiring).toHaveBeenCalled();
    expect(setup.calls.onStatus).toHaveBeenCalledWith(expect.stringContaining("Updated the agent status hooks"));

    // Once, not once per render — and `install` finishes by refreshing the
    // wiring, which is what would otherwise re-enter this effect forever.
    await act(async () => renderer.update(<setup.Harness decision="accepted" adapters={[adapter("claude-code", "partial")]} />));
    expect(setup.calls.applyHooks).toHaveBeenCalledTimes(1);
    await act(async () => renderer.unmount());
  });

  it("stops re-asserting on a host that refuses, instead of rewriting it forever", async () => {
    // `install` refreshes the wiring whatever happens, which produces a new
    // snapshot and re-runs the effect. Without a refusal being remembered, a
    // host whose configuration can never reach `wired` — a racing editor, a
    // stale lock, an unwritable file — is rewritten in a silent loop.
    const applyHooks = vi.fn(async () => { throw new Error("another hook configuration update is in progress"); });
    const setup = harness({ decision: "accepted", adapters: [adapter("claude-code", "partial")], applyHooks });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(applyHooks).toHaveBeenCalledTimes(1);
    expect(setup.calls.onStatus).toHaveBeenCalledWith(expect.stringContaining("Could not update"));
    // Every re-render here hands the hook a *new* adapters array, which is what
    // a fresh snapshot does — and `install` asks for one on its way out.
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await act(async () => renderer.update(<setup.Harness decision="accepted" adapters={[adapter("claude-code", "partial")]} />));
    }
    expect(applyHooks).toHaveBeenCalledTimes(1);
    // A new connection is a new chance.
    await act(async () => renderer.update(<setup.Harness connected={false} decision="accepted" />));
    await act(async () => renderer.update(<setup.Harness decision="accepted" adapters={[adapter("claude-code", "partial")]} />));
    expect(applyHooks).toHaveBeenCalledTimes(2);
    await act(async () => renderer.unmount());
  });

  it("never installs for an adapter the consent dialog did not name", async () => {
    // Consent named the files it would touch. An adapter installed on the host
    // months later was in none of them, so it is offered rather than written.
    const setup = harness({ decision: "accepted", adapters: [adapter("codex", "notWired")] });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(setup.calls.applyHooks).not.toHaveBeenCalled();
    expect(setup.current.offerable).toBe(true);
    await act(async () => renderer.unmount());
  });

  it("writes nothing on a consented host that is already current", async () => {
    const setup = harness({ decision: "accepted", adapters: [adapter("claude-code", "wired")] });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(setup.calls.reviewHooks).not.toHaveBeenCalled();
    expect(setup.calls.applyHooks).not.toHaveBeenCalled();
    expect(setup.current.notice).toBeUndefined();
    await act(async () => renderer.unmount());
  });

  /**
   * M13-E004. The invariant: no agent configuration file is written on a host
   * without an explicit, recorded consent for *that* host, migrations included.
   *
   * What broke it was not a missing check but three independent ones. The
   * decision was read for the host selected when the effect ran, the diff was
   * computed against the host connected when the request went out, and the
   * write went down whichever connection was live when it landed. On a machine
   * that switches hosts — eight app tabs and two profiles, on the field one —
   * those are not the same host, and the field evidence is the shape it
   * leaves: exactly one `hostSetup` entry, "accepted", for the host the user
   * was never prompted about, and none for the host they answered on.
   */
  it("refuses every write and records nothing when the host changes mid-install", async () => {
    // Two adapters, so the switch lands between the first write and the second
    // — the window that is impossible to see and trivial to hit.
    // The runtime stands in for the real one: it refuses a request bound to a
    // host it is no longer connected to, and lets an unbound one through — so
    // dropping the binding fails the assertions below rather than the stub.
    let liveHost = "ssh-omarchy client-1 1";
    const refuseIfMoved = (expectedHost: string | undefined) => {
      if (expectedHost !== undefined && expectedHost !== liveHost) {
        throw new Error("answered for a different host than this app is connected to now");
      }
    };
    const reviewHooks = vi.fn(async (id: string, _action: string, expectedHost?: string) => {
      refuseIfMoved(expectedHost);
      return review(id);
    });
    const applyHooks = vi.fn(async (_review: AgentHookReview, expectedHost?: string) => {
      refuseIfMoved(expectedHost);
      // The switch the user makes while the first adapter is still being written.
      liveHost = "local client-2 1";
    });
    const setup = harness({
      adapters: [adapter("claude-code", "notWired"), adapter("codex", "notWired")],
      reviewHooks,
      applyHooks,
    });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    const accept = renderer.root.findAll((node) => node.type === "button")
      .find((node) => String(node.children[0]).startsWith("Set up this host"))!;
    await act(async () => accept.props.onClick());
    // The first adapter was written to the host the dialog named; the second
    // was refused rather than redirected to the host now connected.
    expect(applyHooks).toHaveBeenCalledTimes(1);
    // And no consent is recorded at all — not for the host that moved away,
    // and above all not for the host that was never asked.
    expect(setup.calls.recordDecision).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("writes nothing when the answer could not be kept", async () => {
    // The app state is write-frozen after a file it could not parse. Installing
    // in that mode leaves configured hooks and, at the next launch, no record
    // of ever having agreed to them — which is half of the shape the field
    // machine was found in.
    const setup = harness({ decisionsArePersistable: false });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    const accept = renderer.root.findAll((node) => node.type === "button")
      .find((node) => String(node.children[0]).startsWith("Set up this host"))!;
    await act(async () => accept.props.onClick());
    expect(setup.calls.reviewHooks).not.toHaveBeenCalled();
    expect(setup.calls.applyHooks).not.toHaveBeenCalled();
    expect(setup.calls.recordDecision).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("answers about the host the dialog named, not the one connected when the button is clicked", async () => {
    // The question names one machine and its configuration paths. An answer to
    // it is an answer about that machine, so the binding is taken when the
    // dialog opens rather than re-derived when it is answered.
    const setup = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    await act(async () => renderer.update(<setup.Harness hostIdentity="local client-2 1" hostProfileId="local" />));
    const decline = renderer.root.findAll((node) => node.type === "button")
      .find((node) => node.children[0] === "Not now")!;
    await act(async () => decline.props.onClick());
    expect(setup.calls.recordDecision).toHaveBeenCalledWith("ssh-omarchy", "declined");
    expect(setup.calls.recordDecision).not.toHaveBeenCalledWith("local", "declined");
    await act(async () => renderer.unmount());
  });

  it("writes nothing on a disconnected host, whatever it last answered", async () => {
    // A migration is still a write. `hostIdentity` absent means there is no
    // connection to bind consent to, so there is nothing to migrate against.
    const setup = harness({ decision: "accepted", hostIdentity: undefined, adapters: [adapter("claude-code", "partial")] });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    expect(setup.calls.reviewHooks).not.toHaveBeenCalled();
    expect(setup.calls.applyHooks).not.toHaveBeenCalled();
    expect(setup.calls.recordDecision).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("takes its question away with the host it was about", async () => {
    const setup = harness();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<setup.Harness />); });
    await act(async () => renderer.update(<setup.Harness connected={false} />));
    expect(renderer.toJSON()).toEqual({ type: "div", props: {}, children: null });
    expect(setup.current.open).toBe(false);
    await act(async () => renderer.unmount());
  });
});

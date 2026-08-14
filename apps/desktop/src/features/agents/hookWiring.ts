import type { AgentAdapterDescriptor, AgentAdapterId, AgentHookWiring } from "./types";

/**
 * What the app is allowed to say about agent status on this host.
 *
 * The field failure this answers: every hook on the user's machine belonged to
 * another tool, so no lifecycle event had ever reached the daemon, and the
 * sidebar rendered process detections — which prove a process exists and
 * nothing else — as if they were states. The app could not tell "nothing is
 * happening" from "nothing here can ever tell me what is happening", so it
 * chose the more flattering one.
 *
 * `reports` is the honest answer to "can this host tell me anything at all".
 * When it is false, the agents section says so instead of implying status it
 * does not have.
 */
export interface HostHookWiring {
  /** At least one adapter's events reach this daemon. */
  reports: boolean;
  /** Adapters that are on this host and whose events do not reach it yet. */
  setupTargets: AgentAdapterDescriptor[];
  /**
   * Why the host could not be read, when that is why it reports nothing.
   * Absent otherwise — a healthy host has no reason to explain itself.
   */
  unreadableReason?: string;
  /** True while the host has not answered — no snapshot, or too old to say. */
  unknown: boolean;
}

const INVITES_SETUP: ReadonlySet<AgentHookWiring> = new Set<AgentHookWiring>(["notWired", "partial"]);

export function hostHookWiring(adapters: readonly AgentAdapterDescriptor[]): HostHookWiring {
  // `absent` adapters are excluded everywhere below. An agent that is not
  // installed on this host has nothing to wire, and treating it as a gap means
  // prompting to create configuration for a vendor the user does not use.
  const hookAdapters = adapters
    .filter((adapter) => adapter.supportsHooks && adapter.hookWiring !== "absent");
  return {
    reports: hookAdapters.some((adapter) => adapter.hookWiring === "wired"),
    setupTargets: hookAdapters.filter((adapter) => INVITES_SETUP.has(adapter.hookWiring)),
    ...hookAdapters
      .filter((adapter) => adapter.hookWiring === "unavailable" && adapter.hookWiringDetail)
      .slice(0, 1)
      .map((adapter) => ({ unreadableReason: adapter.hookWiringDetail }))[0],
    // No adapters at all is "we have not been told", not "nothing is wired":
    // it is what every disconnected and every pre-snapshot render looks like.
    unknown: hookAdapters.length === 0
      || hookAdapters.every((adapter) => adapter.hookWiring === "unspecified"),
  };
}

/**
 * The line the agents section shows when the host cannot report status, and
 * `undefined` when it can. Deliberately one sentence: it replaces guesses, it
 * is not a place to explain hooks — except when the host said *why*, which is
 * the one case where the reason is the whole use of the line.
 */
export function hookWiringNotice(wiring: HostHookWiring): string | undefined {
  if (wiring.reports || wiring.unknown) return undefined;
  if (wiring.setupTargets.length > 0) return "Agent status unavailable on this host — set up hooks";
  return wiring.unreadableReason
    ? `Agent status unavailable on this host — ${wiring.unreadableReason}`
    : "Agent status unavailable on this host — its agent configuration could not be read";
}

/**
 * True when the app should raise the one-time prompt by itself.
 *
 * The question is about the *host*, not about each adapter: a host where Claude
 * Code reports and Codex does not is a host that reports, and interrupting the
 * user to say otherwise is exactly the kind of prompt people learn to dismiss.
 * The remaining gap stays reachable from Settings and from the section's own
 * line, which is where someone who wants it will look.
 */
export function shouldPromptForSetup(wiring: HostHookWiring): boolean {
  return !wiring.reports && !wiring.unknown && wiring.setupTargets.length > 0;
}

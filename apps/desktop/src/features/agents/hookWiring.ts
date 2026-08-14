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
  /** Adapters whose configuration invites an install, in display order. */
  setupTargets: AgentAdapterDescriptor[];
  /** Adapters whose configuration could not be read at all. */
  unreadable: AgentAdapterDescriptor[];
  /** True while the host has not answered — no snapshot, or too old to say. */
  unknown: boolean;
}

const INVITES_SETUP: ReadonlySet<AgentHookWiring> = new Set<AgentHookWiring>(["notWired", "partial"]);

export function hostHookWiring(adapters: readonly AgentAdapterDescriptor[]): HostHookWiring {
  const hookAdapters = adapters.filter((adapter) => adapter.supportsHooks);
  return {
    reports: hookAdapters.some((adapter) => adapter.hookWiring === "wired"),
    setupTargets: hookAdapters.filter((adapter) => INVITES_SETUP.has(adapter.hookWiring)),
    unreadable: hookAdapters.filter((adapter) => adapter.hookWiring === "unavailable"),
    // No adapters at all is "we have not been told", not "nothing is wired":
    // it is what every disconnected and every pre-snapshot render looks like.
    unknown: hookAdapters.length === 0
      || hookAdapters.every((adapter) => adapter.hookWiring === "unspecified"),
  };
}

/**
 * The line the agents section shows when the host cannot report status, and
 * `undefined` when it can. Deliberately one sentence: it replaces guesses, it
 * is not a place to explain hooks.
 */
export function hookWiringNotice(wiring: HostHookWiring): string | undefined {
  if (wiring.reports || wiring.unknown) return undefined;
  if (wiring.setupTargets.length > 0) return "Agent status unavailable on this host — set up hooks";
  return "Agent status unavailable on this host — its agent configuration could not be read";
}

/** Adapter IDs an install would target, in the order they are offered. */
export function setupAdapterIds(wiring: HostHookWiring): AgentAdapterId[] {
  return wiring.setupTargets.map((adapter) => adapter.id);
}

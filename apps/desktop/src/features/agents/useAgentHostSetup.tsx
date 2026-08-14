import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { AgentHostSetupDialog } from "./AgentHostSetupDialog";
import { hostHookWiring, hookWiringNotice, shouldPromptForSetup } from "./hookWiring";
import type { HostSetupDecision } from "../shell/types";
import type { AgentAdapterDescriptor, AgentHookReview, AgentHostNamingOutcome } from "./types";

export interface AgentHostSetupOptions {
  adapters: readonly AgentAdapterDescriptor[];
  /** Live, mutable, authoritative — nothing is offered without all three. */
  connected: boolean;
  hostProfileId: string;
  hostLabel: string;
  /** What the user last answered for this host, if anything. */
  decision?: HostSetupDecision;
  recordDecision(hostProfileId: string, decision: HostSetupDecision): void;
  reviewHooks(adapter: string, action: "install" | "uninstall"): Promise<AgentHookReview>;
  applyHooks(review: AgentHookReview): Promise<void>;
  /**
   * Applies the recommended tmux window naming. Separate from the hooks
   * because it lives in the tmux server's memory rather than a config file,
   * so it has to be re-asserted rather than installed once.
   */
  applyHostNaming(): Promise<AgentHostNamingOutcome>;
  /**
   * Identity of the live connection. A tmux server restart produces a new one,
   * and that is exactly when the in-memory naming has to be re-asserted.
   */
  connectionKey: string;
  /** Re-asks the host what its wiring is now, after a change to it. */
  refreshWiring(): void;
  onStatus(message: string): void;
  /** Opens the existing exact-diff review for one adapter. */
  openReview(adapter: string): void;
}

export interface AgentHostSetup {
  /** Rendered by the shell; null when nothing is being asked. */
  dialog: ReactElement | null;
  /** The shell reads this rather than being told; one source of truth. */
  open: boolean;
  /** The agents section's honest line, or `undefined` when status works. */
  notice?: string;
  /** Whether anything on this host can report what an agent is doing. */
  reports: boolean;
  /** True when Settings should offer to set this host up. */
  offerable: boolean;
  /** Re-opens the prompt from Settings or the sidebar's notice. */
  offer(): void;
}

/**
 * "Set up this host", asked once per host and remembered.
 *
 * The prompt is driven by what the daemon observed, not by whether the app has
 * seen agents: a host with no agent running right now still cannot report one
 * later, and finding that out at the moment an agent finally blocks is exactly
 * the failure this replaces.
 *
 * Two rules keep it from becoming noise. It is asked at most once per host —
 * a recorded "not now" is as durable as a yes, and Settings is where either is
 * revisited. And it is never asked about a configuration the host could not
 * read: that is reported, not overwritten.
 */
export function useAgentHostSetup(options: AgentHostSetupOptions): AgentHostSetup {
  const [open, setOpen] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string>();
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const wiring = useMemo(() => hostHookWiring(options.adapters), [options.adapters]);
  // Two different questions. `offerable` is "is there anything left to set up",
  // which is what Settings and the sidebar's line ask about. Raising the modal
  // unasked needs the stronger one: this host reports nothing at all.
  const offerable = options.connected && wiring.setupTargets.length > 0;
  const promptable = options.connected && shouldPromptForSetup(wiring);

  const offer = useCallback(() => {
    setError(undefined);
    setOpen(true);
  }, []);

  // Asked once, when the host has actually answered. `decision` being undefined
  // is the whole condition: a recorded answer of either kind ends this forever.
  useEffect(() => {
    if (!promptable || options.decision !== undefined) return;
    offer();
  }, [offer, promptable, options.decision]);

  // A host that goes away takes its question with it, rather than leaving a
  // modal over a disconnected app that would act on the next host to connect.
  useEffect(() => {
    if (!options.connected) setOpen(false);
  }, [options.connected]);

  // One key, not a set: connections are sequential, so remembering the last one
  // answers "have I already asserted this on the server I am talking to now",
  // and a set keyed by an ever-incrementing epoch only ever grows.
  const namedConnection = useRef<string | undefined>(undefined);
  const assertNaming = useCallback(() => {
    const current = optionsRef.current;
    if (namedConnection.current === current.connectionKey) return;
    namedConnection.current = current.connectionKey;
    void current.applyHostNaming().then((outcome) => {
      // A change to the user's running tmux server is worth one line; finding
      // that their own config already does it is not.
      if (outcome === "applied") current.onStatus("Recommended tmux window naming applied to this host's tmux server.");
    }).catch((cause) => {
      // Not retried within this connection: nothing that could change the
      // answer happens until the connection does, and the next one has its own
      // key. Non-fatal by design — agent status works without it, and the phase
      // that introduced it declared it non-gating — but never silent.
      current.onStatus(`Recommended tmux window naming was not applied: ${String(cause)}`);
    });
  }, []);

  const install = useCallback((targets: readonly AgentAdapterDescriptor[]) => {
    const current = optionsRef.current;
    setApplying(true);
    setError(undefined);
    return (async () => {
      for (const adapter of targets) {
        const review = await current.reviewHooks(adapter.id, "install");
        // The host's own idempotence answer, so a re-run writes nothing.
        if (!review.alreadyInstalled) await current.applyHooks(review);
      }
    })().then(() => {
      current.recordDecision(current.hostProfileId, "accepted");
      setOpen(false);
      // Part of the same "set up this host" answer, and deliberately after it:
      // a tmux server that refuses the naming must not lose the hooks.
      assertNaming();
      return true;
    }).catch((cause) => {
      setError(String(cause));
      return false;
    }).finally(() => {
      // Unconditionally, including after a failure part-way through: an adapter
      // that was installed before the one that threw *is* wired now, and
      // leaving the dialog claiming otherwise is the lie this phase is about.
      setApplying(false);
      current.refreshWiring();
    });
  }, [assertNaming]);

  // Consent is to keeping this host set up, not to one particular set of hook
  // events. The managed event set moves when a vendor adds an event worth
  // taking, and a host set up before that reads as partially wired — so without
  // this, the sidebar on every already-consented host would say "agent status
  // unavailable" forever and the one-time prompt, already answered, could never
  // come back to fix it. Merge-only, backed up and idempotent, so re-running it
  // on a host that is already current writes nothing at all.
  const reasserted = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (!options.connected || options.decision !== "accepted") return;
    assertNaming();
    if (wiring.setupTargets.length === 0 || reasserted.current === options.connectionKey) return;
    reasserted.current = options.connectionKey;
    const named = wiring.setupTargets.map((adapter) => adapter.displayName).join(" and ");
    void install(wiring.setupTargets).then((ok) => {
      if (ok) optionsRef.current.onStatus(`Updated the agent status hooks for ${named} on this host.`);
    });
  }, [assertNaming, install, options.connected, options.connectionKey, options.decision, wiring.setupTargets]);

  const accept = useCallback(() => {
    const current = optionsRef.current;
    void install(hostHookWiring(current.adapters).setupTargets).then((ok) => {
      if (ok) current.onStatus(`Agent status hooks installed on ${current.hostLabel}.`);
    });
  }, [install]);

  const decline = useCallback(() => {
    const current = optionsRef.current;
    if (current.decision === undefined) current.recordDecision(current.hostProfileId, "declined");
    setOpen(false);
  }, []);

  const dialog = open
    ? <AgentHostSetupDialog
      adapters={wiring.setupTargets}
      applying={applying}
      error={error}
      hostLabel={options.hostLabel}
      onAccept={accept}
      onDecline={decline}
      onReview={() => {
        const first = wiring.setupTargets[0];
        if (!first) return;
        setOpen(false);
        options.openReview(first.id);
      }}
    />
    : null;

  return {
    dialog,
    open,
    notice: hookWiringNotice(wiring),
    reports: wiring.reports,
    offerable,
    offer,
  };
}

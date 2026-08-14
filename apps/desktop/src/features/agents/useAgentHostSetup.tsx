import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { AgentHostSetupDialog } from "./AgentHostSetupDialog";
import { hostHookWiring, hookWiringNotice, type HostHookWiring } from "./hookWiring";
import type { HostSetupDecision } from "../shell/types";
import type { AgentAdapterDescriptor, AgentHookReview } from "./types";

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
  /** Re-asks the host what its wiring is now, after a change to it. */
  refreshWiring(): void;
  onStatus(message: string): void;
  onModalChange(open: boolean): void;
  /** Opens the existing exact-diff review for one adapter. */
  openReview(adapter: string): void;
}

export interface AgentHostSetup {
  /** Rendered by the shell; null when nothing is being asked. */
  dialog: ReactElement | null;
  /** The agents section's honest line, or `undefined` when status works. */
  notice?: string;
  wiring: HostHookWiring;
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
  const offerable = options.connected && wiring.setupTargets.length > 0;

  const close = useCallback((next: boolean) => {
    setOpen(next);
    optionsRef.current.onModalChange(next);
  }, []);

  // Asked once, when the host has actually answered. `decision` being undefined
  // is the whole condition: a recorded answer of either kind ends this forever.
  useEffect(() => {
    if (!offerable || options.decision !== undefined) return;
    setError(undefined);
    close(true);
  }, [close, offerable, options.decision]);

  // A host that goes away takes its question with it, rather than leaving a
  // modal over a disconnected app that would act on the next host to connect.
  useEffect(() => {
    if (!options.connected) close(false);
  }, [close, options.connected]);

  const accept = useCallback(() => {
    const current = optionsRef.current;
    const targets = hostHookWiring(current.adapters).setupTargets;
    setApplying(true);
    setError(undefined);
    void (async () => {
      for (const adapter of targets) {
        const review = await current.reviewHooks(adapter.id, "install");
        // The host's own idempotence answer, so a re-run writes nothing.
        if (!review.alreadyInstalled) await current.applyHooks(review);
      }
    })().then(() => {
      current.recordDecision(current.hostProfileId, "accepted");
      current.refreshWiring();
      close(false);
      current.onStatus(`Agent status hooks installed on ${current.hostLabel}.`);
    }).catch((cause) => {
      // Nothing is recorded on failure: the user has not been asked and
      // answered, they have been shown a broken attempt.
      setError(String(cause));
    }).finally(() => setApplying(false));
  }, [close]);

  const decline = useCallback(() => {
    const current = optionsRef.current;
    if (current.decision === undefined) current.recordDecision(current.hostProfileId, "declined");
    close(false);
  }, [close]);

  const offer = useCallback(() => {
    setError(undefined);
    close(true);
  }, [close]);

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
        close(false);
        options.openReview(first.id);
      }}
    />
    : null;

  return { dialog, notice: hookWiringNotice(wiring), wiring, offerable, offer };
}

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { AgentHostSetupDialog } from "./AgentHostSetupDialog";
import { hostHookWiring, hookWiringNotice, type HostHookWiring } from "./hookWiring";
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

  // Window naming is not installed, it is asserted: it lives in the running
  // tmux server, so a restarted server — a new connection key — silently loses
  // it. Re-sent once per connection, and only where the user already said yes.
  // The host's own detection is what keeps this from overwriting a config the
  // user wrote themselves.
  const namedConnections = useRef(new Set<string>());
  useEffect(() => {
    const current = optionsRef.current;
    if (!current.connected || current.decision !== "accepted") return;
    if (namedConnections.current.has(current.connectionKey)) return;
    namedConnections.current.add(current.connectionKey);
    void current.applyHostNaming().catch((cause) => {
      namedConnections.current.delete(current.connectionKey);
      // Non-fatal by design: agent status works without it, and the phase that
      // introduced it declared it non-gating. It still must not fail silently.
      current.onStatus(`Recommended tmux window naming was not applied: ${String(cause)}`);
    });
  }, [options.connected, options.connectionKey, options.decision]);

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
      // Part of the same "set up this host" answer, and deliberately after it:
      // a tmux server that refuses the naming must not lose the hooks.
      void current.applyHostNaming().then(() => namedConnections.current.add(current.connectionKey))
        .catch((cause) => current.onStatus(`Recommended tmux window naming was not applied: ${String(cause)}`));
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

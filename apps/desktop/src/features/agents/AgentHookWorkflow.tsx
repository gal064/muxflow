import { useCallback, useState, type ReactElement } from "react";
import type { ActiveRoot } from "../files/types";
import { HookReviewDialog } from "./HookReviewDialog";
import type { AgentAdapterId, AgentPlacement, AgentRecord } from "./types";
import type { AgentRuntime } from "./useAgentRuntime";

export interface AgentLaunchContext {
  sessionId: string;
  windowId: string;
  paneId: string;
  root: ActiveRoot;
}

interface AgentWorkflowOptions {
  runtime: AgentRuntime;
  launchContext?: AgentLaunchContext;
  /**
   * The connected host a review is opened against, or `undefined` when there
   * is none. Captured when the diff is computed and re-checked when it is
   * applied: the diff the user read describes one host's file, and a host
   * switch while the dialog is open must refuse the write rather than apply
   * that diff somewhere else (M13-E004).
   */
  host?: { profileId: string; identity: string };
  onStatus(message: string): void;
  onModalChange(open: boolean): void;
  /**
   * Called after this dialog changes a host's hook configuration.
   *
   * Without it, installing through the exact-diff review left the agents
   * section still saying "Agent status unavailable on this host" — the wiring
   * the sidebar reads comes from the snapshot, and nothing had asked the host
   * for a new one. The one-time setup prompt refreshed and this path did not,
   * which is precisely the kind of divergence two flows accumulate.
   *
   * `action` matters: removing this app's hooks through the review is the user
   * withdrawing their consent to keep this host set up. Without that, the
   * per-host "accepted" would put the hooks straight back on the next connect,
   * and the uninstall the user just confirmed would silently not stick.
   *
   * `hostProfileId` is the host the review was opened against, not whichever
   * one is connected when it lands.
   */
  onHooksChanged(action: "install" | "uninstall", hostProfileId: string, hostIdentity: string): void;
}

export interface AgentWorkflow {
  launch(adapter: AgentAdapterId, placement: AgentPlacement): void;
  resume(agent: AgentRecord, placement: AgentPlacement): void;
  rename(agent: AgentRecord, displayName: string): void;
  reviewHooks(adapter: AgentAdapterId, action: "install" | "uninstall"): void;
  /** Opens a review that was already loaded for the captured setup host. */
  openHookReview(
    review: Awaited<ReturnType<AgentRuntime["reviewHooks"]>>,
    host: { profileId: string; identity: string },
  ): void;
  /** The dialog to render; null when no review is open. */
  dialog: ReactElement | null;
}

/**
 * Agent lifecycle actions, without a panel around them.
 *
 * The agents panel this replaces carried two disclosure menus, a rename button
 * and a per-row `•••` on every row, permanently docked in 258px of window. The
 * actions themselves did not go anywhere — they are reached from the sidebar's
 * context menus now — so what is left here is the part that genuinely needs
 * state: the hook review, which shows an exact diff of a config file the app is
 * about to write and cannot be a fire-and-forget menu item.
 */
export function useAgentWorkflow(options: AgentWorkflowOptions): AgentWorkflow {
  const { runtime, launchContext, host, onStatus, onModalChange, onHooksChanged } = options;
  // The host is stored *with* the diff: the two are one answer about one
  // machine, and separating them is what let the second be applied to a
  // different first.
  const [review, setReviewState] = useState<{
    diff: Awaited<ReturnType<AgentRuntime["reviewHooks"]>>;
    host: NonNullable<AgentWorkflowOptions["host"]>;
  }>();
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string>();

  const setReview = useCallback((next: typeof review) => {
    setReviewState(next);
    onModalChange(Boolean(next));
  }, [onModalChange]);

  const adapterName = (adapterId: string) =>
    runtime.adapters.find((item) => item.id === adapterId)?.displayName ?? adapterId;

  const launch = useCallback((adapter: AgentAdapterId, placement: AgentPlacement) => {
    if (!launchContext) return onStatus("Choose a live terminal pane with an authoritative active root before launching an agent.");
    void runtime.launch({
      adapterId: adapter, placement, sessionId: launchContext.sessionId, windowId: launchContext.windowId,
      paneId: launchContext.paneId, activeRoot: launchContext.root.path, rootToken: launchContext.root.token,
    }).then(() => onStatus(`Launching ${adapterName(adapter)} in a new ${placement}…`))
      .catch((cause) => onStatus(String(cause)));
  }, [launchContext, onStatus, runtime]);

  const resume = useCallback((agent: AgentRecord, placement: AgentPlacement) => {
    if (!launchContext) return onStatus("Choose a live terminal pane with an authoritative active root before resuming an agent.");
    void runtime.resume(agent, {
      adapterId: agent.adapterId, placement, sessionId: launchContext.sessionId, windowId: launchContext.windowId,
      paneId: launchContext.paneId, activeRoot: launchContext.root.path, rootToken: launchContext.root.token,
    }).then(() => onStatus(`Resuming ${agent.displayName} in a new ${placement}…`))
      .catch((cause) => onStatus(String(cause)));
  }, [launchContext, onStatus, runtime]);

  const rename = useCallback((agent: AgentRecord, displayName: string) => {
    void runtime.rename(agent, displayName)
      .then(() => onStatus(`Renaming agent to ${displayName}…`))
      .catch((cause) => onStatus(String(cause)));
  }, [onStatus, runtime]);

  const reviewHooks = useCallback((adapter: AgentAdapterId, action: "install" | "uninstall") => {
    if (!host) {
      onStatus("Reviewing hook changes requires a live authoritative host.");
      return;
    }
    void runtime.reviewHooks(adapter, action, host.identity).then((diff) => {
      setError(undefined);
      setReview({ diff, host });
    }).catch((cause) => onStatus(String(cause)));
  }, [host, onStatus, runtime, setReview]);

  const openHookReview = useCallback((diff: Awaited<ReturnType<AgentRuntime["reviewHooks"]>>, reviewedHost: {
    profileId: string;
    identity: string;
  }) => {
    setError(undefined);
    setReview({ diff, host: reviewedHost });
  }, [setReview]);

  const dialog = review ? <HookReviewDialog
    applying={applying}
    error={error}
    review={review.diff}
    onCancel={() => { if (!applying) setReview(undefined); }}
    onConfirm={() => {
      setApplying(true);
      setError(undefined);
      void runtime.applyHooks(review.diff, review.host.identity).then(() => {
        onStatus(`${adapterName(review.diff.adapterId)} reviewed hooks ${review.diff.action === "install" ? "installed" : "removed"}.`);
        onHooksChanged(review.diff.action, review.host.profileId, review.host.identity);
        setReview(undefined);
      }).catch((cause) => setError(String(cause))).finally(() => setApplying(false));
    }}
  /> : null;

  return { launch, resume, rename, reviewHooks, openHookReview, dialog };
}

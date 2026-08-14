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
  onStatus(message: string): void;
  onModalChange(open: boolean): void;
}

export interface AgentWorkflow {
  launch(adapter: AgentAdapterId, placement: AgentPlacement): void;
  resume(agent: AgentRecord, placement: AgentPlacement): void;
  rename(agent: AgentRecord, displayName: string): void;
  reviewHooks(adapter: AgentAdapterId, action: "install" | "uninstall"): void;
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
  const { runtime, launchContext, onStatus, onModalChange } = options;
  const [review, setReviewState] = useState<Awaited<ReturnType<AgentRuntime["reviewHooks"]>>>();
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
    void runtime.reviewHooks(adapter, action).then((next) => {
      setError(undefined);
      setReview(next);
    }).catch((cause) => onStatus(String(cause)));
  }, [onStatus, runtime, setReview]);

  const dialog = review ? <HookReviewDialog
    applying={applying}
    error={error}
    review={review}
    onCancel={() => { if (!applying) setReview(undefined); }}
    onConfirm={() => {
      setApplying(true);
      setError(undefined);
      void runtime.applyHooks(review).then(() => {
        onStatus(`${adapterName(review.adapterId)} reviewed hooks ${review.action === "install" ? "installed" : "removed"}.`);
        setReview(undefined);
      }).catch((cause) => setError(String(cause))).finally(() => setApplying(false));
    }}
  /> : null;

  return { launch, resume, rename, reviewHooks, dialog };
}

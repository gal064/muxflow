import { useCallback, type MutableRefObject } from "react";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import {
  requestReconciledTmuxAction,
  type ReconciledTmuxActionOptions,
} from "../features/tmux/actionReconciliation";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import {
  abandonPanePaintSpan,
  openPanePaintSpan,
  targetPanePaintSpan,
  type PanePaintSpan,
} from "../perf/probe";

const INTERACTION_SPAN_BY_ACTION: Partial<Record<TmuxAction["kind"], PanePaintSpan>> = {
  createSession: "create.workspace",
  createWindow: "create.tab",
  selectWindow: "window.switch",
  splitPaneDown: "pane.split",
  splitPaneRight: "pane.split",
};

export type TmuxActionExecution = {
  kind: "navigation";
  feedback: "visible" | "silent";
  measurePanePaint: boolean;
};

interface Options {
  canMutate: boolean;
  clientId?: string;
  generation: number;
  hostScopeRef: MutableRefObject<HostScopeToken>;
  reconciliation?: Pick<ReconciledTmuxActionOptions, "request" | "waitForNewerScope">;
  requestAction?: typeof requestReconciledTmuxAction;
  serverIdentity?: string;
  setStatus(status: string): void;
}

/** Scope-safe action execution and its explicit UI/performance reporting policy. */
export function useTmuxActionPerformer(options: Options) {
  return useCallback(async (
    action: TmuxAction,
    capturedPrecondition?: { serverIdentity: string; generation: number },
    execution?: TmuxActionExecution,
  ): Promise<TmuxActionResult | undefined> => {
    const reportStatus = execution?.kind !== "navigation" || execution.feedback === "visible";
    if (!options.clientId || !options.canMutate || !options.serverIdentity) {
      if (reportStatus) options.setStatus("This action is unavailable until the authoritative connection is live.");
      return undefined;
    }
    const initialScope = options.hostScopeRef.current;
    const paneSpan = execution?.kind === "navigation" && !execution.measurePanePaint
      ? undefined
      : INTERACTION_SPAN_BY_ACTION[action.kind];
    const paneSpanHandle = paneSpan ? openPanePaintSpan(paneSpan, options.clientId) : undefined;
    try {
      const result = await (options.requestAction ?? requestReconciledTmuxAction)({
        clientId: options.clientId,
        action,
        capturedPrecondition,
        initialScope,
        currentScope: () => options.hostScopeRef.current,
        ...options.reconciliation,
      });
      if (!sameHostConnection(initialScope, options.hostScopeRef.current)) {
        abandonPanePaintSpan(paneSpanHandle);
        return undefined;
      }
      targetPanePaintSpan(paneSpanHandle, result.paneId);
      if (reportStatus) options.setStatus("Waiting for authoritative tmux state…");
      return result;
    } catch (error) {
      abandonPanePaintSpan(paneSpanHandle);
      if (reportStatus && sameHostConnection(initialScope, options.hostScopeRef.current)) options.setStatus(String(error));
      if (execution?.kind === "navigation") throw error;
      return undefined;
    }
  }, [options.canMutate, options.clientId, options.generation, options.hostScopeRef,
    options.reconciliation, options.requestAction, options.serverIdentity, options.setStatus]);
}

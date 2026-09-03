import { useCallback, type MutableRefObject } from "react";
import { requestTmuxAction, type TmuxAction, type TmuxActionResult } from "../features/tmux/actions";
import {
  requestReconciledTmuxAction,
  type ReconciledTmuxActionOptions,
} from "../features/tmux/actionReconciliation";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import { recordIncident } from "../diagnostics/incidents";
import {
  abandonPanePaintSpan,
  openPanePaintSpan,
  recordPerfRecord,
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

/**
 * A host other than the one on screen, for an action a row on that host
 * asked for. The scope is read live, like the active host's ref: the
 * result is discarded if that host's connection moved on under the request.
 * A missing client means the host has no live bridge, and the action is
 * refused the way it is on the active host before its bridge is up.
 */
export interface TmuxActionTarget {
  clientId?: string;
  canMutate: boolean;
  scopeRef: { readonly current: HostScopeToken };
}

/** Scope-safe action execution and its explicit UI/performance reporting policy. */
export function useTmuxActionPerformer(options: Options) {
  return useCallback(async (
    action: TmuxAction,
    capturedPrecondition?: { serverIdentity: string; generation: number },
    execution?: TmuxActionExecution,
    target?: TmuxActionTarget,
  ): Promise<TmuxActionResult | undefined> => {
    const reportStatus = execution?.kind !== "navigation" || execution.feedback === "visible";
    const clientId = target ? target.clientId : options.clientId;
    const canMutate = target ? target.canMutate : options.canMutate;
    const scopeRef = target ? target.scopeRef : options.hostScopeRef;
    const serverIdentity = target ? scopeRef.current.serverIdentity : options.serverIdentity;
    if (!clientId || !canMutate || !serverIdentity) {
      if (reportStatus) options.setStatus("This action is unavailable until the authoritative connection is live.");
      return undefined;
    }
    const initialScope = scopeRef.current;
    const paneSpan = execution?.kind === "navigation" && !execution.measurePanePaint
      ? undefined
      : INTERACTION_SPAN_BY_ACTION[action.kind];
    const paneSpanHandle = paneSpan ? openPanePaintSpan(paneSpan, clientId) : undefined;
    // The renderer's two ends of the switch timeline. Recorded for every
    // action, not only slow ones: the question being asked is how a switch on a
    // fast link differs from one on a slow one, and that needs the fast
    // baseline in the same log. `d1` is wall clock because the other four
    // stamps are taken on two different machines and only a shared epoch joins
    // them; `sentAt` stays monotonic because `elapsedMs` must not move when the
    // clock does. This is a perf-log record, not an incident: it is measurement
    // and it is inert unless the process opted in.
    const d1 = Date.now();
    const sentAt = performance.now();
    try {
      const result = await (options.requestAction ?? requestReconciledTmuxAction)({
        clientId,
        action,
        capturedPrecondition,
        initialScope,
        currentScope: () => scopeRef.current,
        ...options.reconciliation,
        // A peer's round trip says nothing about the link the user types
        // over, so it is not measured as if it did.
        ...(target && !options.reconciliation?.request
          ? { request: (id: string, act: TmuxAction, precondition: { serverIdentity: string; generation: number }) => requestTmuxAction(id, act, precondition, false) }
          : {}),
      });
      recordPerfRecord("perf.timeline", {
        action: action.kind,
        // d2..d5 and the head-of-line counters, exactly as native measured
        // them. Absent when the native half is not compiled in, which leaves a
        // record that still brackets the round trip with d1/d6.
        ...result.timing,
        d1,
        // D6: this caller resumed. `d1` to `d6` spans the whole reconciliation,
        // including any stale-topology retry, while `d2`..`d5` describe the
        // final attempt only.
        d6: Date.now(),
        elapsedMs: Math.round(performance.now() - sentAt),
        ok: true,
      });
      if (!sameHostConnection(initialScope, scopeRef.current)) {
        abandonPanePaintSpan(paneSpanHandle);
        return undefined;
      }
      targetPanePaintSpan(paneSpanHandle, result.paneId);
      if (reportStatus) options.setStatus("Waiting for authoritative tmux state…");
      return result;
    } catch (error) {
      abandonPanePaintSpan(paneSpanHandle);
      // Every refusal, whoever is listening. A status string is read by whatever
      // renders next and then gone — and a bulk close overwrote its own with the
      // following tab's — so the only record of "the host said no" was a message
      // nobody kept. One line per refusal: refusals are exceptional, and the
      // largest burst is one per tab in a bulk close.
      recordIncident("action.refused", { action: action.kind, error: String(error).slice(0, 200) });
      // A refusal carries no native timing — the invoke rejected — so the
      // record is the renderer's bracket plus the reason.
      recordPerfRecord("perf.timeline", {
        action: action.kind,
        d1,
        d6: Date.now(),
        elapsedMs: Math.round(performance.now() - sentAt),
        ok: false,
        code: String(error).slice(0, 120),
      });
      if (reportStatus && sameHostConnection(initialScope, scopeRef.current)) options.setStatus(String(error));
      if (execution?.kind === "navigation") throw error;
      return undefined;
    }
  }, [options.canMutate, options.clientId, options.generation, options.hostScopeRef,
    options.reconciliation, options.requestAction, options.serverIdentity, options.setStatus]);
}

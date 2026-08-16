import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";

export type AppRecoveryScope = HostScopeToken & { serverIdentity: string };

type AppRecoveryFields = {
  count: number;
  previousServerIdentity: string;
  scope: AppRecoveryScope;
};

export type AppRecoveryState = AppRecoveryFields & (
  | { phase: "offered" }
  | { phase: "confirmingDiscard" }
);

export type AppRecoveryDiscardState = AppRecoveryFields & { phase: "confirmingDiscard" };

export type AppRecoveryAction =
  | { type: "offer"; count: number; previousServerIdentity: string; scope: AppRecoveryScope }
  | { type: "confirmDiscard" }
  | { type: "cancelDiscard" }
  | { type: "reconcileScope"; scope: HostScopeToken }
  | { type: "clear" };

/** One reducer owns both the offer and its modal; no independent open flag exists. */
export function appRecoveryReducer(
  state: AppRecoveryState | undefined,
  action: AppRecoveryAction,
): AppRecoveryState | undefined {
  switch (action.type) {
    case "offer":
      return {
        count: action.count,
        phase: "offered",
        previousServerIdentity: action.previousServerIdentity,
        scope: action.scope,
      };
    case "confirmDiscard":
      return state ? { ...state, phase: "confirmingDiscard" } : undefined;
    case "cancelDiscard":
      return state ? { ...state, phase: "offered" } : undefined;
    case "reconcileScope":
      return state && sameHostConnection(state.scope, action.scope) ? state : undefined;
    case "clear":
      return undefined;
  }
}

export function appRecoveryDiscardState(state: AppRecoveryState | undefined): AppRecoveryDiscardState | undefined {
  return state?.phase === "confirmingDiscard" ? state : undefined;
}

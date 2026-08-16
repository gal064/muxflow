import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";

export type AppRecoveryOffer = {
  hostProfileId: string;
  previousServerIdentity: string;
  currentServerIdentity: string;
  count: number;
  scope: HostScopeToken;
};

export type AppRecoveryState = AppRecoveryOffer & (
  | { phase: "offered" }
  | { phase: "confirmingDiscard" }
);

export type AppRecoveryDiscardState = AppRecoveryOffer & { phase: "confirmingDiscard" };

export function offerAppRecovery(offer: AppRecoveryOffer): AppRecoveryState {
  return { ...offer, phase: "offered" };
}

export function confirmAppRecoveryDiscard(state: AppRecoveryState): AppRecoveryState {
  return { ...state, phase: "confirmingDiscard" };
}

export function cancelAppRecoveryDiscard(state: AppRecoveryState): AppRecoveryState {
  return { ...state, phase: "offered" };
}

/** Topology churn keeps the offer; only replacement of its durable connection clears it. */
export function reconcileAppRecovery(
  state: AppRecoveryState | undefined,
  currentScope: HostScopeToken,
): AppRecoveryState | undefined {
  return state && sameHostConnection(state.scope, currentScope) ? state : undefined;
}

export function appRecoveryModalOpen(state: AppRecoveryState | undefined): boolean {
  return appRecoveryDiscardState(state) !== undefined;
}

export function appRecoveryDiscardState(
  state: AppRecoveryState | undefined,
): AppRecoveryDiscardState | undefined {
  return state?.phase === "confirmingDiscard" ? state : undefined;
}

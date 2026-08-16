import { useCallback, useEffect, useReducer, useRef, type Dispatch, type SetStateAction } from "react";
import {
  discardServerAppState,
  reconcileWorkspaceIdentity,
  recoverableAppTabCount,
  recoverAppTabsFromPreviousServer,
} from "../features/shell/model";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import type { PersistedAppState } from "../features/shell/types";
import type { Session } from "./types";
import { appRecoveryDiscardState, appRecoveryReducer } from "./appRecovery";

interface AppRecoveryControllerOptions {
  appState: PersistedAppState;
  currentHostProfileId: string;
  currentScope: HostScopeToken;
  serverIdentity?: string;
  sessions: readonly Session[];
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
}

/** Owns replacement-server reconciliation and the complete recovery dialog lifecycle. */
export function useAppRecoveryController(options: AppRecoveryControllerOptions) {
  const [state, dispatch] = useReducer(appRecoveryReducer, undefined);
  const lastIdentity = useRef<{ hostProfileId: string; serverIdentity?: string } | undefined>(undefined);
  const scopeRef = useRef(options.currentScope);
  const sessionsRef = useRef(options.sessions);
  scopeRef.current = options.currentScope;
  sessionsRef.current = options.sessions;

  useEffect(() => {
    const previous = lastIdentity.current;
    if (previous?.serverIdentity && options.serverIdentity
      && previous.hostProfileId === options.currentHostProfileId
      && previous.serverIdentity !== options.serverIdentity) {
      const count = recoverableAppTabCount(
        options.appState,
        options.currentHostProfileId,
        previous.serverIdentity,
        options.sessions,
      );
      if (count > 0) dispatch({
        type: "offer",
        count,
        previousServerIdentity: previous.serverIdentity,
        scope: { ...options.currentScope, serverIdentity: options.serverIdentity },
      });
    }
    options.setAppState((current) => reconcileWorkspaceIdentity(
      current,
      options.currentHostProfileId,
      options.serverIdentity,
      options.sessions,
    ));
    // A reconnect intentionally clears the live host state before its new
    // ServerHello arrives. Preserve the last non-empty identity across that
    // gap for the same profile so A -> undefined -> B can still offer recovery;
    // never carry it across a profile change.
    if (options.serverIdentity || previous?.hostProfileId !== options.currentHostProfileId) {
      lastIdentity.current = {
        hostProfileId: options.currentHostProfileId,
        serverIdentity: options.serverIdentity,
      };
    }
  }, [options.currentHostProfileId, options.serverIdentity, options.sessions]);

  useEffect(() => {
    dispatch({ type: "reconcileScope", scope: options.currentScope });
  }, [
    options.currentScope.connectionEpoch,
    options.currentScope.connectionKey,
    options.currentScope.hostProfileId,
    options.currentScope.serverIdentity,
  ]);

  const restore = useCallback(() => {
    if (!state || !sameHostConnection(state.scope, scopeRef.current)) return dispatch({ type: "clear" });
    options.setAppState((current) => recoverAppTabsFromPreviousServer(
      current,
      state.scope.hostProfileId,
      state.previousServerIdentity,
      state.scope.serverIdentity,
      sessionsRef.current,
    ));
    dispatch({ type: "clear" });
  }, [options.setAppState, state]);

  const discard = useCallback(() => {
    if (!state || !sameHostConnection(state.scope, scopeRef.current)) return dispatch({ type: "clear" });
    options.setAppState((current) => discardServerAppState(
      current,
      state.scope.hostProfileId,
      state.previousServerIdentity,
    ));
    dispatch({ type: "clear" });
  }, [options.setAppState, state]);

  return {
    cancelDiscard: () => dispatch({ type: "cancelDiscard" }),
    confirmDiscard: discard,
    dialog: appRecoveryDiscardState(state),
    modalOpen: state?.phase === "confirmingDiscard",
    offer: state,
    requestDiscard: () => dispatch({ type: "confirmDiscard" }),
    restore,
  };
}

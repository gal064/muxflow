import { useCallback, useEffect, useMemo, useRef } from "react";
import type { Pane, Session, Window as TmuxWindow } from "./types";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import type { TmuxActionExecution } from "./useTmuxActionPerformer";
import { shellNavigationMode } from "../features/shell/model";
import {
  destinationLocation,
  RemoteNavigationCoordinator,
  shellTransitionPlan,
  type NavigationLocation,
  type NavigationOutcome,
  type NavigationPrecondition,
  type ShellDestination,
} from "./shellNavigationCoordinator";

export { RemoteNavigationCoordinator, shellTransitionPlan } from "./shellNavigationCoordinator";
export type { ShellDestination } from "./shellNavigationCoordinator";

type Precondition = NavigationPrecondition;

export interface PaneSurfaceResult {
  ok: boolean;
  error?: unknown;
}

export type PaneNavigationFeedback =
  | { kind: "silent" }
  | { kind: "announce"; source: string; successMessage?: string };

export interface ShellNavigationOptions {
  activeSessionId?: string;
  activeWindowId?: string;
  canMutate: boolean;
  currentScope: HostScopeToken;
  focusPaneController(paneId: string): void;
  performAction(
    action: TmuxAction,
    precondition?: Precondition,
    execution?: TmuxActionExecution,
  ): Promise<TmuxActionResult | undefined>;
  sessions: readonly Session[];
  setActiveSessionId(sessionId: string): void;
  setActiveWindowId(windowId: string | undefined): void;
  setAppTab(sessionId: string, appTabId: string | undefined): void;
  setStatus(status: string): void;
  windows: readonly TmuxWindow[];
  acknowledgeHostSessionSelection?(sessionId: string): void;
}

export class ShellNavigationSupersededError extends Error {
  constructor() {
    super("navigation was superseded by a newer destination");
    this.name = "ShellNavigationSupersededError";
  }
}

export function reportAnnouncedPaneResult(
  result: PaneSurfaceResult,
  setStatus: (status: string) => void,
): void {
  if (!result.ok && !(result.error instanceof ShellNavigationSupersededError)) {
    setStatus(result.error instanceof Error ? result.error.message : String(result.error));
  }
}

export function commitScopedAppTabClose(options: {
  activeWindowId?: string;
  commit(): void;
  currentScope: HostScopeToken;
  selectedAppTabId?: string;
  scope: HostScopeToken;
  tabId: string;
  tabSessionId: string;
  revealTerminal(sessionId: string, windowId: string | undefined, commit: () => void): void;
}): void {
  if (!sameHostConnection(options.scope, options.currentScope)) return;
  if (options.selectedAppTabId === options.tabId) {
    options.revealTerminal(options.tabSessionId, options.activeWindowId, options.commit);
  } else {
    options.commit();
  }
}

export function useShellNavigation(options: ShellNavigationOptions) {
  const coordinator = useMemo(() => new RemoteNavigationCoordinator(), []);
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const {
    connectionEpoch, connectionKey, hostProfileId, serverIdentity,
  } = options.currentScope;
  const creationVersion = useRef(0);
  const scopeRef = useRef(options.currentScope);
  const activeSessionIdRef = useRef(options.activeSessionId);
  const activeWindowIdRef = useRef(options.activeWindowId);
  const sessionsRef = useRef(options.sessions);
  const windowsRef = useRef(options.windows);
  const protectedAppTab = useRef<{
    appTabId: string;
    scope: HostScopeToken;
    sessionId: string;
    throughGeneration?: number;
    windowId?: string;
    lastObservation?: { generation: number; windowId?: string };
  } | undefined>(undefined);
  scopeRef.current = optionsRef.current.currentScope;
  activeSessionIdRef.current = optionsRef.current.activeSessionId;
  activeWindowIdRef.current = optionsRef.current.activeWindowId;
  sessionsRef.current = optionsRef.current.sessions;
  windowsRef.current = optionsRef.current.windows;

  useEffect(() => {
    coordinator.invalidate();
    protectedAppTab.current = undefined;
  }, [connectionEpoch, connectionKey, coordinator, hostProfileId, serverIdentity]);

  const scopeCurrent = useCallback(
    (scope: HostScopeToken) => sameHostConnection(scope, scopeRef.current),
    [],
  );
  const requestLocation = useCallback(async (
    destination: ShellDestination,
    predecessor: NavigationOutcome | undefined,
    isCurrent: () => boolean,
    scope: HostScopeToken,
    precondition?: Precondition,
    measureWindowPaint = true,
  ): Promise<NavigationOutcome> => {
    if (!scopeCurrent(scope)) return { kind: "unknown", reason: "scope" };
    if (!scope.serverIdentity) return { kind: "unknown", reason: "request" };
    const target = destinationLocation(destination);
    const targetSessionAlreadyActive = target.sessionId === activeSessionIdRef.current;
    const targetWindowAlreadyActive = Boolean(target.windowId)
      && windowsRef.current.some((item) => item.id === target.windowId && item.active);
    const currentLocation: NavigationOutcome | undefined = !predecessor && activeSessionIdRef.current
      ? {
        kind: "partial",
        location: { sessionId: activeSessionIdRef.current, windowId: activeWindowIdRef.current },
        generation: scope.generation,
        generationSource: "snapshot",
      }
      : undefined;
    const plan = shellTransitionPlan(
      predecessor ?? currentLocation, destination, targetWindowAlreadyActive, targetSessionAlreadyActive,
    );
    const predecessorGeneration = predecessor?.kind === "reached" || predecessor?.kind === "partial"
      ? predecessor.generation : undefined;
    let generation = predecessorGeneration ?? precondition?.generation ?? scope.generation;
    let generationSource: "action" | "snapshot" = predecessor?.kind === "reached" || predecessor?.kind === "partial"
      ? predecessor.generationSource
      : precondition ? "action" : "snapshot";
    let nextPrecondition = predecessorGeneration === undefined
      ? predecessor ? undefined : precondition
      : { serverIdentity: scope.serverIdentity, generation: predecessorGeneration };
    let appliedLocation: NavigationLocation | undefined = predecessor?.kind === "reached"
      ? destinationLocation(predecessor.destination)
      : predecessor?.kind === "partial" ? predecessor.location : undefined;
    if (plan.selectSession) {
      let selected: TmuxActionResult | undefined;
      try {
        selected = await optionsRef.current.performAction(
          { kind: "selectSession", sessionId: target.sessionId },
          nextPrecondition,
          { kind: "navigation", feedback: measureWindowPaint ? "visible" : "silent", measurePanePaint: false },
        );
      } catch (error) {
        return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
      }
      if (!selected) return {
        kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope",
      };
      if (!scopeCurrent(scope)) return { kind: "unknown", reason: "scope" };
      optionsRef.current.acknowledgeHostSessionSelection?.(target.sessionId);
      generation = selected.topologyGeneration;
      generationSource = "action";
      appliedLocation = { sessionId: target.sessionId };
      nextPrecondition = { serverIdentity: scope.serverIdentity, generation };
      if (!isCurrent() && plan.selectWindow) {
        return { kind: "partial", location: appliedLocation, generation, generationSource };
      }
    }
    if (plan.selectWindow && target.windowId) {
      let selected: TmuxActionResult | undefined;
      try {
        selected = await optionsRef.current.performAction(
          { kind: "selectWindow", sessionId: target.sessionId, windowId: target.windowId },
          nextPrecondition,
          {
            kind: "navigation", feedback: measureWindowPaint ? "visible" : "silent",
            measurePanePaint: measureWindowPaint,
          },
        );
      } catch (error) {
        return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
      }
      if (!selected) return {
        kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope",
      };
      if (!scopeCurrent(scope)) return { kind: "unknown", reason: "scope" };
      generation = selected.topologyGeneration;
      generationSource = "action";
      appliedLocation = { sessionId: target.sessionId, windowId: target.windowId };
    }
    if (!scopeCurrent(scope)) return { kind: "unknown", reason: "scope" };
    if (!isCurrent() && appliedLocation
      && (appliedLocation.sessionId !== target.sessionId || appliedLocation.windowId !== target.windowId)) {
      return { kind: "partial", location: appliedLocation, generation, generationSource };
    }
    return { kind: "reached", destination, generation, generationSource };
  }, [scopeCurrent]);

  const beginTerminalIntent = useCallback(() => {
    protectedAppTab.current = undefined;
  }, []);
  const selectSession = useCallback((sessionId: string) => {
    beginTerminalIntent();
    if (shellNavigationMode(optionsRef.current.canMutate) === "cached") {
      coordinator.navigateLocal({
        destination: { kind: "session", sessionId },
        request: async () => ({
          kind: "reached", destination: { kind: "session", sessionId }, generation: scopeRef.current.generation,
          generationSource: "snapshot",
        }),
        commit: () => {
          activeSessionIdRef.current = sessionId;
          activeWindowIdRef.current = undefined;
          optionsRef.current.setAppTab(sessionId, undefined);
          optionsRef.current.setActiveSessionId(sessionId);
          optionsRef.current.setStatus("Viewing the last known workspace. Writes remain frozen.");
        },
      });
      return;
    }
    const scope = scopeRef.current;
    void coordinator.navigate({
      destination: { kind: "session", sessionId },
      request: async (predecessor, isCurrent) => (await requestLocation(
        { kind: "session", sessionId }, predecessor, isCurrent, scope,
      )),
      commit: () => {
        if (!scopeCurrent(scope)) return;
        activeSessionIdRef.current = sessionId;
        activeWindowIdRef.current = undefined;
        optionsRef.current.setAppTab(sessionId, undefined);
        optionsRef.current.setActiveSessionId(sessionId);
      },
    }, sessionId === activeSessionIdRef.current
      ? { kind: "reached", destination: { kind: "session", sessionId }, generation: scope.generation, generationSource: "snapshot" }
      : undefined);
  }, [beginTerminalIntent, coordinator, requestLocation, scopeCurrent]);

  const selectWindowDestination = useCallback((destination: Extract<ShellDestination, { kind: "window" }>) => {
    const scope = scopeRef.current;
    return coordinator.navigate({
      destination,
      request: async (predecessor, isCurrent) => (await requestLocation(
        destination, predecessor, isCurrent, scope, destination.precondition,
      )),
      commit: () => {
        if (!scopeCurrent(scope)) return;
        activeSessionIdRef.current = destination.sessionId;
        activeWindowIdRef.current = destination.windowId;
        optionsRef.current.setAppTab(destination.sessionId, undefined);
        optionsRef.current.setActiveSessionId(destination.sessionId);
        optionsRef.current.setActiveWindowId(destination.windowId);
      },
    }, destination.windowId === activeWindowIdRef.current && destination.sessionId === activeSessionIdRef.current
      ? { kind: "reached", destination, generation: scope.generation, generationSource: "snapshot" }
      : undefined);
  }, [coordinator, requestLocation, scopeCurrent]);

  const selectWindow = useCallback((windowId: string) => {
    beginTerminalIntent();
    const target = windowsRef.current.find((window) => window.id === windowId);
    if (!target) return;
    if (shellNavigationMode(optionsRef.current.canMutate) === "cached") {
      coordinator.navigateLocal({
        destination: { kind: "window", sessionId: target.sessionId, windowId: target.id },
        request: async () => ({
          kind: "reached",
          destination: { kind: "window", sessionId: target.sessionId, windowId: target.id },
          generation: scopeRef.current.generation,
          generationSource: "snapshot",
        }),
        commit: () => {
          activeSessionIdRef.current = target.sessionId;
          activeWindowIdRef.current = target.id;
          optionsRef.current.setAppTab(target.sessionId, undefined);
          optionsRef.current.setActiveSessionId(target.sessionId);
          optionsRef.current.setActiveWindowId(target.id);
          optionsRef.current.setStatus("Viewing the last known terminal tab. Writes remain frozen.");
        },
      });
      return;
    }
    void selectWindowDestination({ kind: "window", sessionId: target.sessionId, windowId: target.id });
  }, [beginTerminalIntent, coordinator, selectWindowDestination]);

  const createWindow = useCallback((sessionId: string) => {
    beginTerminalIntent();
    const scope = scopeRef.current;
    const key = `create-window:${++creationVersion.current}`;
    let created: TmuxActionResult | undefined;
    let createdSessionId = sessionId;
    void coordinator.navigate({
      destination: { kind: "operation", key },
      request: async () => {
        try {
          created = await optionsRef.current.performAction({ kind: "createWindow", sessionId });
        } catch (error) {
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
        }
        if (!created?.windowId || !scopeCurrent(scope)) {
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope" };
        }
        createdSessionId = created.sessionId ?? sessionId;
        optionsRef.current.acknowledgeHostSessionSelection?.(createdSessionId);
        return {
          kind: "reached",
          destination: { kind: "window", sessionId: createdSessionId, windowId: created.windowId },
          generation: created.topologyGeneration,
          generationSource: "action",
        };
      },
      commit: () => {
        if (!scopeCurrent(scope) || !created?.windowId) return;
        activeSessionIdRef.current = createdSessionId;
        activeWindowIdRef.current = created.windowId;
        optionsRef.current.setAppTab(createdSessionId, undefined);
        optionsRef.current.setActiveSessionId(createdSessionId);
        optionsRef.current.setActiveWindowId(created.windowId);
      },
    });
  }, [beginTerminalIntent, coordinator, scopeCurrent]);

  const selectLocalAppTab = useCallback((
    sessionId: string,
    windowId: string | undefined,
    appTabId: string,
    commitLocal: () => void,
  ) => {
    const scope = scopeRef.current;
    const protectFromPendingRemote = coordinator.hasRemoteFlight();
    if (protectFromPendingRemote) protectedAppTab.current = { appTabId, scope, sessionId, windowId };
    void coordinator.navigateLocal({
      destination: { kind: "appTab", sessionId, windowId, appTabId },
      request: async (predecessor, isCurrent) => {
        const result = await requestLocation(
          { kind: "appTab", sessionId, windowId, appTabId }, predecessor, isCurrent, scope, undefined, false,
        );
        const protection = protectedAppTab.current;
        if (protection?.appTabId === appTabId && sameHostConnection(protection.scope, scope)) {
          if (result.kind === "reached") {
            protection.throughGeneration = result.generation;
            const observed = protection.lastObservation;
            if (observed && observed.generation >= result.generation
              && (!protection.windowId || protection.windowId === observed.windowId)) {
              protectedAppTab.current = undefined;
            }
          }
          else protectedAppTab.current = undefined;
        }
        return result;
      },
      commit: () => { if (scopeCurrent(scope)) commitLocal(); },
    });
  }, [coordinator, requestLocation, scopeCurrent]);

  const selectAppTab = useCallback((sessionId: string, windowId: string | undefined, appTabId: string) => {
    selectLocalAppTab(sessionId, windowId, appTabId, () => optionsRef.current.setAppTab(sessionId, appTabId));
  }, [selectLocalAppTab]);

  const revealLocalTerminal = useCallback((
    sessionId: string,
    windowId: string | undefined,
    commitLocal: () => void,
  ) => {
    beginTerminalIntent();
    const scope = scopeRef.current;
    const destination: ShellDestination = windowId
      ? { kind: "window", sessionId, windowId }
      : { kind: "session", sessionId };
    void coordinator.navigateLocal({
      destination,
      request: (predecessor, isCurrent) => requestLocation(
        destination, predecessor, isCurrent, scope, undefined, false,
      ),
      commit: () => { if (scopeCurrent(scope)) commitLocal(); },
    });
  }, [beginTerminalIntent, coordinator, requestLocation]);

  const selectPane = useCallback(async (target: Pane, feedback: PaneNavigationFeedback): Promise<PaneSurfaceResult> => {
    beginTerminalIntent();
    const source = feedback.kind === "announce" ? feedback.source : "Pane";
    if (!sessionsRef.current.some((session) => session.id === target.sessionId)) {
      const error = new Error(`${source} destination is no longer available.`);
      if (feedback.kind === "announce") optionsRef.current.setStatus(error.message);
      return { ok: false, error };
    }
    const scope = scopeRef.current;
    const destination: ShellDestination = {
      kind: "pane",
      sessionId: target.sessionId,
      windowId: target.windowId,
      paneId: target.id,
    };
    const accepted = await coordinator.navigate({
      destination,
      request: async (predecessor, isCurrent) => {
        if (!sessionsRef.current.some((session) => session.id === target.sessionId)) {
          return { kind: "unknown", reason: "request" };
        }
        const location = await requestLocation(destination, predecessor, isCurrent, scope, undefined, false);
        if (location.kind !== "reached") return location;
        if (!scopeCurrent(scope)) return { kind: "unknown", reason: "scope" };
        if (!isCurrent()) return location;
        let focused: TmuxActionResult | undefined;
        try {
          focused = await optionsRef.current.performAction({
            kind: "focusPane",
            sessionId: target.sessionId,
            windowId: target.windowId,
            paneId: target.id,
          }, location.generationSource === "action"
            ? { serverIdentity: scope.serverIdentity!, generation: location.generation }
            : undefined, {
            kind: "navigation", feedback: "silent", measurePanePaint: false,
          });
        } catch (error) {
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
        }
        if (!focused) {
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope" };
        }
        return {
          kind: "reached", destination, generation: focused.topologyGeneration, generationSource: "action",
        };
      },
      commit: () => {
        if (!scopeCurrent(scope)) return;
        if (!sessionsRef.current.some((session) => session.id === target.sessionId)) return;
        activeSessionIdRef.current = target.sessionId;
        activeWindowIdRef.current = target.windowId;
        optionsRef.current.setAppTab(target.sessionId, undefined);
        optionsRef.current.setActiveSessionId(target.sessionId);
        optionsRef.current.setActiveWindowId(target.windowId);
        if (feedback.kind === "announce") optionsRef.current.setStatus(feedback.successMessage
          ? `${feedback.successMessage} Focus request accepted.`
          : `${feedback.source} focus request accepted for ${target.sessionId}/${target.windowId}/${target.id}.`);
        window.requestAnimationFrame(() => optionsRef.current.focusPaneController(target.id));
      },
    });
    if (accepted.kind === "reached") return { ok: true };
    if (accepted.kind !== "unknown" || accepted.reason === "scope" || accepted.reason === "superseded") {
      return { ok: false, error: new ShellNavigationSupersededError() };
    }
    return { ok: false, error: accepted.error ?? new Error(`${source} focus request was not accepted.`) };
  }, [beginTerminalIntent, coordinator, requestLocation, scopeCurrent]);

  const createSession = useCallback((name: string) => {
    beginTerminalIntent();
    const scope = scopeRef.current;
    const key = `create-session:${++creationVersion.current}`;
    let created: TmuxActionResult | undefined;
    void coordinator.navigate({
      destination: { kind: "operation", key },
      request: async () => {
        try {
          created = await optionsRef.current.performAction({ kind: "createSession", name });
        } catch (error) {
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
        }
        if (!created?.sessionId || !scopeCurrent(scope)) {
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope" };
        }
        optionsRef.current.acknowledgeHostSessionSelection?.(created.sessionId);
        return {
          kind: "reached",
          destination: { kind: "session", sessionId: created.sessionId },
          generation: created.topologyGeneration,
          generationSource: "action",
        };
      },
      commit: () => {
        if (!scopeCurrent(scope) || !created?.sessionId) return;
        activeSessionIdRef.current = created.sessionId;
        activeWindowIdRef.current = undefined;
        optionsRef.current.setAppTab(created.sessionId, undefined);
        optionsRef.current.setActiveSessionId(created.sessionId);
        optionsRef.current.setActiveWindowId(undefined);
      },
    });
  }, [beginTerminalIntent, coordinator, scopeCurrent]);

  const observeAuthoritativeWindow = useCallback((sessionId: string, windowId: string | undefined, generation: number) => {
    const protection = protectedAppTab.current;
    if (!protection || !sameHostConnection(protection.scope, scopeRef.current)) return false;
    if (protection.sessionId !== sessionId) return true;
    protection.lastObservation = { generation, windowId };
    if (protection.throughGeneration !== undefined
      && generation >= protection.throughGeneration
      && (!protection.windowId || protection.windowId === windowId)) {
      protectedAppTab.current = undefined;
    }
    return true;
  }, []);

  return useMemo(() => ({
    createSession, createWindow, observeAuthoritativeWindow, selectAppTab, selectLocalAppTab,
    selectPane, selectSession, selectWindow, revealLocalTerminal,
  }), [createSession, createWindow, observeAuthoritativeWindow, revealLocalTerminal, selectAppTab,
    selectLocalAppTab, selectPane, selectSession, selectWindow]);
}

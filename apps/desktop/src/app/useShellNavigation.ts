import { useCallback, useEffect, useMemo, useRef, type MutableRefObject } from "react";
import type { Pane, Session, Window as TmuxWindow } from "./types";
import { sameHostConnection, type HostScopeToken } from "../features/shell/hostScope";
import { windowForSession, type OptimisticWindowSwitch } from "./windowSelection";
import type { TmuxAction, TmuxActionResult } from "../features/tmux/actions";
import type { TmuxActionExecution } from "./useTmuxActionPerformer";
import { shellNavigationMode, type PendingShellTab } from "../features/shell/model";
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
  /**
   * Publishes the placeholder tab for a create that is still in flight, or
   * `undefined` to withdraw it. See `PendingShellTab`.
   */
  setPendingTab?(pending: PendingShellTab | undefined): void;
  /**
   * The snap-back guard for an optimistic window or workspace switch, owned by
   * `useAppConnectionController` because that is where snapshots decide the
   * active window. Absent means switches stay ack-gated.
   */
  optimisticWindow?: MutableRefObject<OptimisticWindowSwitch | undefined>;
}

/** What a workspace create carries beyond its name. See `createSession`. */
export interface CreateSessionOptions {
  /** Where the first pane starts. The host resolves and validates it. */
  directory?: string;
  /**
   * Called once, with the ack's authoritative identity, after the ack has been
   * validated and before the shell commits the switch. The scope is the one
   * the request was issued in, so the caller can refuse to act on a workspace
   * that belongs to a connection the app has since left.
   */
  onCreated?(
    created: { sessionId: string; windowId?: string; paneId: string; topologyGeneration: number },
    scope: HostScopeToken,
  ): void;
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
  /**
   * The create whose placeholder is on screen, if any.
   *
   * Keyed so that a slow create failing cannot withdraw the placeholder a
   * newer create has already put up: two clicks in quick succession is the
   * ordinary case, and the older request's answer arriving second is exactly
   * when a bare boolean would clear the wrong one.
   */
  const pendingTabKey = useRef<string | undefined>(undefined);
  scopeRef.current = optionsRef.current.currentScope;
  activeSessionIdRef.current = optionsRef.current.activeSessionId;
  activeWindowIdRef.current = optionsRef.current.activeWindowId;
  sessionsRef.current = optionsRef.current.sessions;
  windowsRef.current = optionsRef.current.windows;

  useEffect(() => {
    coordinator.invalidate();
    protectedAppTab.current = undefined;
    // A create against the previous connection can never be answered now, so
    // its placeholder would sit in the strip forever.
    if (pendingTabKey.current !== undefined) {
      pendingTabKey.current = undefined;
      optionsRef.current.setPendingTab?.(undefined);
    }
  }, [connectionEpoch, connectionKey, coordinator, hostProfileId, serverIdentity, optionsRef]);

  const scopeCurrent = useCallback(
    (scope: HostScopeToken) => sameHostConnection(scope, scopeRef.current),
    [],
  );

  const publishPendingTab = useCallback((scope: HostScopeToken, pending: PendingShellTab) => {
    if (!sameHostConnection(scope, scopeRef.current)) return;
    pendingTabKey.current = pending.key;
    optionsRef.current.setPendingTab?.(pending);
  }, [optionsRef]);

  /** Withdraws the placeholder only if it is still the one this create put up. */
  const withdrawPendingTab = useCallback((key: string) => {
    if (pendingTabKey.current !== key) return;
    pendingTabKey.current = undefined;
    optionsRef.current.setPendingTab?.(undefined);
  }, [optionsRef]);
  const requestLocation = useCallback(async (
    destination: ShellDestination,
    predecessor: NavigationOutcome | undefined,
    isCurrent: () => boolean,
    scope: HostScopeToken,
    precondition?: Precondition,
    measureWindowPaint = true,
    /**
     * Where to plan the transition *from*.
     *
     * Defaults to the shell's current location, which is right for every
     * ack-gated caller. An optimistic switch has to pass it explicitly: it has
     * already moved the shell to the destination, so planning from the shell
     * would conclude there is nowhere to go and send nothing at all — the UI
     * would show the new window while tmux still sat on the old one.
     */
    origin?: { sessionId?: string; windowId?: string },
  ): Promise<NavigationOutcome> => {
    if (!scopeCurrent(scope)) return { kind: "unknown", reason: "scope" };
    if (!scope.serverIdentity) return { kind: "unknown", reason: "request" };
    const target = destinationLocation(destination);
    const from = origin ?? { sessionId: activeSessionIdRef.current, windowId: activeWindowIdRef.current };
    const targetSessionAlreadyActive = target.sessionId === from.sessionId;
    const targetWindowAlreadyActive = Boolean(target.windowId)
      && windowsRef.current.some((item) => item.id === target.windowId && item.active);
    const currentLocation: NavigationOutcome | undefined = !predecessor && from.sessionId
      ? {
        kind: "partial",
        location: { sessionId: from.sessionId, windowId: from.windowId },
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
    const alreadyThere = sessionId === activeSessionIdRef.current;
    // Read before the optimistic commit moves them. This is where the host
    // still is, and so what the transition has to be planned from — and where
    // a refused switch has to put the shell back.
    const origin = { sessionId: activeSessionIdRef.current, windowId: activeWindowIdRef.current };
    // Resolved here, from the same snapshot the controller's post-paint effect
    // would read, so the workspace and the window it shows change in one
    // commit. Leaving it to the effect paints the new workspace against the
    // old workspace's window first, which resolves to no active window at all.
    // Undefined where this connection has never seen the workspace's windows;
    // the effect still resolves that one when the snapshot arrives.
    const windowId = windowForSession(windowsRef.current, sessionId, origin.windowId);
    const commit = () => {
      if (!scopeCurrent(scope)) return;
      activeSessionIdRef.current = sessionId;
      activeWindowIdRef.current = windowId;
      optionsRef.current.setAppTab(sessionId, undefined);
      optionsRef.current.setActiveSessionId(sessionId);
      optionsRef.current.setActiveWindowId(windowId);
    };
    // The switch is painted now and the request reconciles behind it. Only
    // where the guard exists to hold it, for the same reason window switches
    // need it: the snapshots arriving during the round trip still describe the
    // workspace being left, and would resolve the window out from under it.
    const guard = optionsRef.current.optimisticWindow;
    const optimistic = Boolean(guard) && !alreadyThere && scopeCurrent(scope);
    if (optimistic && guard) {
      guard.current = { sessionId, windowId };
      commit();
    }
    /** Whether the guard still holds *this* switch rather than a newer one. */
    const owns = () => guard?.current?.sessionId === sessionId && guard.current.windowId === windowId;
    /** Puts the shell back where the host actually is, after a switch that failed. */
    const rollback = () => {
      if (!guard || !owns()) return;
      guard.current = undefined;
      if (!scopeCurrent(scope) || !origin.sessionId) return;
      // Actively, not by waiting: a refused switch changes nothing on the
      // host, so there may be no further snapshot to correct the UI with. The
      // status the user sees is the performer's, as on the window path.
      const restored = windowForSession(windowsRef.current, origin.sessionId, origin.windowId);
      activeSessionIdRef.current = origin.sessionId;
      activeWindowIdRef.current = restored;
      optionsRef.current.setActiveSessionId(origin.sessionId);
      optionsRef.current.setActiveWindowId(restored);
    };
    void coordinator.navigate({
      destination: { kind: "session", sessionId },
      request: async (predecessor, isCurrent) => {
        const result = await requestLocation(
          { kind: "session", sessionId }, predecessor, isCurrent, scope, undefined, true,
          optimistic ? origin : undefined,
        );
        if (optimistic && guard) {
          if (result.kind === "reached" || result.kind === "partial") {
            // Held until a snapshot has caught up: the ack is not the
            // snapshot, and releasing on the ack alone reopens the very gap
            // this guard exists to cover.
            if (owns()) guard.current = { sessionId, windowId, throughGeneration: result.generation };
          } else rollback();
        }
        return result;
      },
      commit,
    }, alreadyThere
      ? { kind: "reached", destination: { kind: "session", sessionId }, generation: scope.generation, generationSource: "snapshot" }
      : undefined);
  }, [beginTerminalIntent, coordinator, requestLocation, scopeCurrent]);

  const selectWindowDestination = useCallback((destination: Extract<ShellDestination, { kind: "window" }>) => {
    const scope = scopeRef.current;
    const alreadyThere = destination.windowId === activeWindowIdRef.current
      && destination.sessionId === activeSessionIdRef.current;
    // Read before the optimistic commit moves them. This is where the host
    // still is, and so what the transition has to be planned from.
    const origin = { sessionId: activeSessionIdRef.current, windowId: activeWindowIdRef.current };
    const commit = () => {
      if (!scopeCurrent(scope)) return;
      activeSessionIdRef.current = destination.sessionId;
      activeWindowIdRef.current = destination.windowId;
      optionsRef.current.setAppTab(destination.sessionId, undefined);
      optionsRef.current.setActiveSessionId(destination.sessionId);
      optionsRef.current.setActiveWindowId(destination.windowId);
    };
    // The switch is painted now and the request reconciles behind it. Only
    // where the guard exists to hold it: without something to stop the next
    // snapshot naming the *old* window as active, an optimistic commit is
    // reverted within one snapshot, which is worse than waiting.
    const guard = optionsRef.current.optimisticWindow;
    const optimistic = Boolean(guard) && !alreadyThere && scopeCurrent(scope);
    if (optimistic && guard) {
      guard.current = { sessionId: destination.sessionId, windowId: destination.windowId };
      commit();
    }
    /** Puts the shell back where the host actually is, after a switch that failed. */
    const rollback = () => {
      if (!guard) return;
      guard.current = undefined;
      if (!scopeCurrent(scope)) return;
      // Actively, not by waiting: a refused switch changes nothing on the
      // host, so there may be no further snapshot to correct the UI with.
      const authoritative = windowsRef.current.find((window) => window.active);
      if (!authoritative) return;
      activeSessionIdRef.current = authoritative.sessionId;
      activeWindowIdRef.current = authoritative.id;
      optionsRef.current.setActiveSessionId(authoritative.sessionId);
      optionsRef.current.setActiveWindowId(authoritative.id);
    };
    return coordinator.navigate({
      destination,
      request: async (predecessor, isCurrent) => {
        const result = await requestLocation(
          destination, predecessor, isCurrent, scope, destination.precondition, true,
          optimistic ? origin : undefined,
        );
        if (optimistic && guard) {
          if (result.kind === "reached" || result.kind === "partial") {
            // Held until a snapshot has caught up: the ack is not the
            // snapshot, and releasing on the ack alone reopens the very gap
            // this guard exists to cover.
            if (guard.current?.windowId === destination.windowId) {
              guard.current = { ...guard.current, throughGeneration: result.generation };
            }
          } else rollback();
        }
        return result;
      },
      commit,
    }, alreadyThere
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
    // Before the request, not after: the whole point is that the strip changes
    // in the same frame as the click rather than a round trip later.
    publishPendingTab(scope, { key, sessionId, title: "New window" });
    void coordinator.navigate({
      destination: { kind: "operation", key },
      request: async () => {
        try {
          created = await optionsRef.current.performAction({ kind: "createWindow", sessionId });
        } catch (error) {
          withdrawPendingTab(key);
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
        }
        if (!created?.windowId || !scopeCurrent(scope)) {
          withdrawPendingTab(key);
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope" };
        }
        // Upgraded rather than withdrawn: the ack names the window but the
        // snapshot that contains it has not arrived, and dropping the
        // placeholder here would blink the strip empty in between. Nothing on
        // the success path ever withdraws it — retirement belongs to the latch
        // in `App`, which clears it once the workspace's window list names this
        // window (`retirePendingTab`). Withdrawal here stays for the failures
        // above, where no window is ever coming.
        publishPendingTab(scope, {
          key, sessionId: created.sessionId ?? sessionId, windowId: created.windowId, title: "New window",
        });
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
  }, [beginTerminalIntent, coordinator, publishPendingTab, scopeCurrent, withdrawPendingTab]);

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

  const createSession = useCallback((name: string, options?: CreateSessionOptions) => {
    beginTerminalIntent();
    const scope = scopeRef.current;
    const key = `create-session:${++creationVersion.current}`;
    let created: TmuxActionResult | undefined;
    // No `sessionId` yet, so nothing is drawn until the ack names the new
    // workspace. A placeholder in the workspace being left behind would point
    // at the wrong strip; the gap this closes is the one *after* the switch,
    // where the new workspace is on screen and its snapshot has not landed.
    publishPendingTab(scope, { key, title: name || "New session" });
    void coordinator.navigate({
      destination: { kind: "operation", key },
      request: async () => {
        try {
          created = await optionsRef.current.performAction({
            kind: "createSession", name, ...(options?.directory ? { directory: options.directory } : {}),
          });
        } catch (error) {
          withdrawPendingTab(key);
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope", error };
        }
        if (!created?.sessionId || !scopeCurrent(scope)) {
          withdrawPendingTab(key);
          return { kind: "unknown", reason: scopeCurrent(scope) ? "request" : "scope" };
        }
        // The ack's window id is what lets this placeholder retire: the retire
        // rule is "the window I stand in for exists in the snapshot" — applied
        // once, and for good, by the latch in `App` (`retirePendingTab`) — and
        // a placeholder without one waits forever.
        publishPendingTab(scope, {
          key, sessionId: created.sessionId, windowId: created.windowId, title: name || "New session",
        });
        optionsRef.current.acknowledgeHostSessionSelection?.(created.sessionId);
        // The one moment a workspace's startup command can be delivered: the
        // ack has just named the pane, the dispatcher has already attached a
        // control client to it, and the scope has been re-checked since the
        // request left. Nothing about this is retried or remembered — a
        // reconnect, a restart, a replay or a reselect goes nowhere near here,
        // which is what "exactly once, in the workspace that was created"
        // means. A create that answers with no pane gets nothing sent.
        const paneId = created.paneId;
        if (paneId) {
          options?.onCreated?.({
            sessionId: created.sessionId,
            windowId: created.windowId,
            paneId,
            topologyGeneration: created.topologyGeneration,
          }, scope);
        }
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
        // The ack names the new session's one window, so the workspace and its
        // window commit together — the same single-paint rule the optimistic
        // switch follows.
        activeWindowIdRef.current = created.windowId;
        optionsRef.current.setAppTab(created.sessionId, undefined);
        optionsRef.current.setActiveSessionId(created.sessionId);
        optionsRef.current.setActiveWindowId(created.windowId);
      },
    });
  }, [beginTerminalIntent, coordinator, publishPendingTab, scopeCurrent, withdrawPendingTab]);

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

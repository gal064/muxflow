export type NavigationPrecondition = { serverIdentity: string; generation: number };

export type ShellDestination =
  | { kind: "session"; sessionId: string }
  | { kind: "window"; sessionId: string; windowId: string; precondition?: NavigationPrecondition }
  | { kind: "pane"; sessionId: string; windowId: string; paneId: string }
  | { kind: "appTab"; sessionId: string; windowId?: string; appTabId: string };

export interface NavigationLocation { sessionId: string; windowId?: string }
export type NavigationOutcome =
  | { kind: "reached"; destination: ShellDestination; generation: number; generationSource: "action" | "snapshot" }
  | { kind: "partial"; location: NavigationLocation; generation: number; generationSource: "action" | "snapshot" }
  | { kind: "unknown"; reason: "request" | "scope" | "superseded"; error?: unknown };

interface NavigationIntent {
  commit(): void;
  destination: ShellDestination | { kind: "operation"; key: string };
  request(predecessor: NavigationOutcome | undefined, isCurrent: () => boolean): Promise<NavigationOutcome>;
}

export interface ShellTransitionPlan {
  selectSession: boolean;
  selectWindow: boolean;
}

export function destinationLocation(destination: ShellDestination): NavigationLocation {
  return {
    sessionId: destination.sessionId,
    windowId: destination.kind === "window" || destination.kind === "pane" || destination.kind === "appTab"
      ? destination.windowId
      : undefined,
  };
}

/** The minimum authoritative location changes required after a known predecessor. */
export function shellTransitionPlan(
  predecessor: NavigationOutcome | undefined,
  destination: ShellDestination,
  targetWindowAlreadyActive = false,
  targetSessionAlreadyActive = true,
): ShellTransitionPlan {
  const target = destinationLocation(destination);
  if (!predecessor) {
    return {
      selectSession: destination.kind === "session" || !targetSessionAlreadyActive,
      selectWindow: Boolean(target.windowId) && !targetWindowAlreadyActive,
    };
  }
  if (predecessor.kind === "unknown") {
    return { selectSession: true, selectWindow: Boolean(target.windowId) };
  }
  const previous = predecessor.kind === "reached"
    ? destinationLocation(predecessor.destination)
    : predecessor.location;
  const changesSession = previous.sessionId !== target.sessionId;
  return {
    selectSession: changesSession,
    selectWindow: Boolean(target.windowId) && previous.windowId !== target.windowId
      && !((changesSession || previous.windowId === undefined) && targetWindowAlreadyActive),
  };
}

interface PendingIntent extends NavigationIntent {
  settle(outcome: NavigationOutcome): void;
}

function destinationKey(destination: NavigationIntent["destination"]): string {
  switch (destination.kind) {
    case "session": return `session:${destination.sessionId}`;
    case "window": return `window:${destination.sessionId}:${destination.windowId}`;
    case "pane": return `pane:${destination.sessionId}:${destination.windowId}:${destination.paneId}`;
    case "appTab": return `app:${destination.sessionId}:${destination.appTabId}`;
    case "operation": return `operation:${destination.key}`;
  }
}

/** Serializes every shell destination and retains only the latest user intent. */
export class RemoteNavigationCoordinator {
  #desired?: PendingIntent;
  #flight?: { intent: PendingIntent };

  navigate(intent: NavigationIntent, alreadySelected?: NavigationOutcome): Promise<NavigationOutcome> {
    const pending = this.#pending(intent);
    this.#replaceDesired(pending);
    if (this.#flight) return pending.promise;
    if (alreadySelected) {
      this.#desired = undefined;
      pending.commit();
      pending.settle(alreadySelected);
      return pending.promise;
    }
    this.#start(pending, undefined);
    return pending.promise;
  }

  /** Local tabs paint immediately, then reassert their terminal location after an older remote flight. */
  navigateLocal(intent: NavigationIntent): Promise<NavigationOutcome> | undefined {
    intent.commit();
    if (!this.#flight) return undefined;
    const pending = this.#pending(intent);
    this.#replaceDesired(pending);
    return pending.promise;
  }

  hasRemoteFlight(): boolean {
    return Boolean(this.#flight);
  }

  invalidate(): void {
    const flight = this.#flight?.intent;
    const desired = this.#desired;
    this.#desired = undefined;
    this.#flight = undefined;
    flight?.settle({ kind: "unknown", reason: "scope" });
    if (desired !== flight) desired?.settle({ kind: "unknown", reason: "scope" });
  }

  #pending(intent: NavigationIntent): PendingIntent & { promise: Promise<NavigationOutcome> } {
    let finish!: (outcome: NavigationOutcome) => void;
    const promise = new Promise<NavigationOutcome>((resolve) => { finish = resolve; });
    let settled = false;
    return {
      ...intent,
      promise,
      settle: (outcome) => {
        if (settled) return;
        settled = true;
        finish(outcome);
      },
    };
  }

  #replaceDesired(intent: PendingIntent): void {
    const previous = this.#desired;
    this.#desired = intent;
    if (previous && previous !== this.#flight?.intent) previous.settle({ kind: "unknown", reason: "superseded" });
  }

  #start(intent: PendingIntent, predecessor: NavigationOutcome | undefined): void {
    const flight = { intent };
    this.#flight = flight;
    void intent.request(predecessor, () => this.#flight === flight && this.#desired === intent).then(
      (outcome) => this.#finish(flight, outcome),
      (error) => this.#finish(flight, { kind: "unknown", reason: "request", error }),
    );
  }

  #finish(flight: { intent: PendingIntent }, outcome: NavigationOutcome): void {
    if (this.#flight !== flight) return flight.intent.settle({ kind: "unknown", reason: "superseded" });
    this.#flight = undefined;
    const desired = this.#desired;
    if (!desired) return flight.intent.settle({ kind: "unknown", reason: "superseded" });
    if (destinationKey(desired.destination) === destinationKey(flight.intent.destination)) {
      this.#desired = undefined;
      if (outcome.kind === "reached") desired.commit();
      if (desired !== flight.intent) flight.intent.settle({ kind: "unknown", reason: "superseded" });
      desired.settle(outcome);
      return;
    }
    flight.intent.settle({ kind: "unknown", reason: "superseded" });
    this.#start(desired, outcome);
  }
}

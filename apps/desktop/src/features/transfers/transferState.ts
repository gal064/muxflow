/**
 * Canonical renderer transfer lifecycle. Uploads and downloads must use this
 * exact vocabulary so a backend terminal outcome can never remain styled as
 * active merely because one surface did not know about it.
 */
export type TransferState =
  | "queued"
  | "preflighting"
  | "running"
  | "verifying"
  | "completed"
  | "cancelled"
  | "failed";

export type TransferOutcome = "published" | "notPublished" | "unknown";
export type TransferFailureKind = "staleScope" | "transfer" | "timeout" | "outcomeUnknown" | "cleanup";
export type TransferCleanupStatus = "notNeeded" | "removed" | "retained" | "failed" | "connectionClosed";

export interface CanonicalTransferSnapshot {
  state: TransferState;
  outcome?: TransferOutcome;
  failureKind?: TransferFailureKind;
  cleanupStatus?: TransferCleanupStatus;
  cleanupError?: string;
  error?: string;
}

/** Translate the exhaustive backend wire vocabulary without type assertions. */
export function transferStateFromWire(state: string): TransferState {
  switch (state) {
    case "queued": return "queued";
    case "preflighting": return "preflighting";
    case "running": return "running";
    case "verifying": return "verifying";
    case "completed": return "completed";
    case "cancelled": return "cancelled";
    case "failed": return "failed";
    default: throw new Error(`Unknown transfer state: ${state}`);
  }
}

export function transferOutcomeFromWire(outcome: string | undefined, state: TransferState): TransferOutcome | undefined {
  if (outcome === undefined || outcome === "") {
    if (isTerminalTransferState(state)) throw new Error(`Terminal transfer state ${state} omitted its outcome.`);
    return undefined;
  }
  switch (outcome) {
    case "published": return "published";
    case "notPublished": return "notPublished";
    case "unknown": return "unknown";
    default: throw new Error(`Unknown transfer outcome: ${outcome}`);
  }
}

export function isTerminalTransferState(state: TransferState): boolean {
  switch (state) {
    case "queued":
    case "preflighting":
    case "running":
    case "verifying": return false;
    case "completed":
    case "cancelled":
    case "failed": return true;
  }
}

/** Once verification/commit begins the backend owns the authoritative outcome. */
export function canCancelTransfer(state: TransferState): boolean {
  switch (state) {
    case "queued":
    case "preflighting":
    case "running": return true;
    case "verifying":
    case "completed":
    case "cancelled":
    case "failed": return false;
  }
}

export function transferStateLabel(state: TransferState): string {
  switch (state) {
    case "queued": return "Queued";
    case "preflighting": return "Checking destination";
    case "running": return "Transferring";
    case "verifying": return "Verifying and committing";
    case "completed": return "Completed";
    case "cancelled": return "Cancelled";
    case "failed": return "Failed";
  }
}

export function transferFailureKindFromWire(value: string | undefined): TransferFailureKind | undefined {
  if (value === undefined || value === "") return undefined;
  switch (value) {
    case "staleScope": return "staleScope";
    case "transfer": return "transfer";
    case "timeout": return "timeout";
    case "outcomeUnknown": return "outcomeUnknown";
    case "cleanup": return "cleanup";
    default: throw new Error(`Unknown transfer failure kind: ${value}`);
  }
}

export function transferCleanupStatusFromWire(value: string | undefined): TransferCleanupStatus | undefined {
  if (value === undefined || value === "") return undefined;
  switch (value) {
    case "notNeeded": return "notNeeded";
    case "removed": return "removed";
    case "retained": return "retained";
    case "failed": return "failed";
    case "connectionClosed": return "connectionClosed";
    default: throw new Error(`Unknown transfer cleanup status: ${value}`);
  }
}

export function validateTransferStateOutcome(
  state: TransferState,
  outcome: TransferOutcome | undefined,
  failureKind: TransferFailureKind | undefined,
): void {
  if (!isTerminalTransferState(state)) {
    if (outcome !== undefined || failureKind !== undefined) throw new Error("An active transfer reported a terminal outcome.");
    return;
  }
  if (outcome === undefined) throw new Error(`Terminal transfer state ${state} omitted its outcome.`);
  if (state === "completed" && outcome === "unknown") throw new Error("A completed transfer cannot have an unknown outcome.");
  if (state === "cancelled" && outcome !== "notPublished") throw new Error("A cancelled transfer must be notPublished.");
  if (state === "failed" && failureKind === undefined) throw new Error("A failed transfer omitted its failure kind.");
  if (state !== "failed" && failureKind !== undefined) throw new Error("Only a failed transfer can carry a failure kind.");
  const permitsUnknownOutcome = failureKind === "outcomeUnknown" || failureKind === "timeout" || failureKind === "staleScope";
  if (outcome === "unknown" && !permitsUnknownOutcome) {
    throw new Error("An unknown publication outcome requires outcomeUnknown, timeout, or staleScope failure kind.");
  }
  if (failureKind === "outcomeUnknown" && outcome !== "unknown") {
    throw new Error("An outcomeUnknown failure must carry an unknown publication outcome.");
  }
}

const CLEANUP_SEVERITY: Record<TransferCleanupStatus, number> = {
  notNeeded: 0,
  removed: 1,
  connectionClosed: 2,
  retained: 3,
  failed: 4,
};

/** Merge sequenced and late transfer frames without weakening lifecycle or cleanup truth. */
export function mergeCanonicalTransfer<T extends CanonicalTransferSnapshot>(current: T, incoming: T): T {
  const lifecycle = lifecycleMayAdvance(current.state, incoming.state) ? incoming : current;
  const cleanup = mergeCleanup(current, incoming);
  const merged = { ...lifecycle };
  delete merged.cleanupStatus;
  delete merged.cleanupError;
  if (cleanup.cleanupStatus) merged.cleanupStatus = cleanup.cleanupStatus;
  if (cleanup.cleanupError) merged.cleanupError = cleanup.cleanupError;
  return merged;
}

function lifecycleMayAdvance(current: TransferState, incoming: TransferState): boolean {
  if (isTerminalTransferState(current)) return false;
  if (isTerminalTransferState(incoming)) return true;
  return activeStateOrder(incoming) >= activeStateOrder(current);
}

function activeStateOrder(state: TransferState): number {
  switch (state) {
    case "queued": return 0;
    case "preflighting": return 1;
    case "running": return 2;
    case "verifying": return 3;
    case "completed":
    case "cancelled":
    case "failed": throw new Error(`Terminal state ${state} has no active order.`);
  }
}

function mergeCleanup(
  current: Pick<CanonicalTransferSnapshot, "cleanupStatus" | "cleanupError">,
  incoming: Pick<CanonicalTransferSnapshot, "cleanupStatus" | "cleanupError">,
): Pick<CanonicalTransferSnapshot, "cleanupStatus" | "cleanupError"> {
  if (!incoming.cleanupStatus) return current;
  if (!current.cleanupStatus || CLEANUP_SEVERITY[incoming.cleanupStatus] > CLEANUP_SEVERITY[current.cleanupStatus]) {
    return { cleanupStatus: incoming.cleanupStatus, ...(incoming.cleanupError ? { cleanupError: incoming.cleanupError } : {}) };
  }
  if (CLEANUP_SEVERITY[incoming.cleanupStatus] < CLEANUP_SEVERITY[current.cleanupStatus]) return current;
  return {
    cleanupStatus: current.cleanupStatus,
    ...(current.cleanupError || incoming.cleanupError ? { cleanupError: current.cleanupError || incoming.cleanupError } : {}),
  };
}

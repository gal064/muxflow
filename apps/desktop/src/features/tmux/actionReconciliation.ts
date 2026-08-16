import { sameHostConnection, type HostScopeToken } from "../shell/hostScope";
import {
  isDestructiveTmuxAction,
  isStaleTmuxTopologyError,
  requestTmuxAction,
  type AuthoritativePrecondition,
  type TmuxAction,
  type TmuxActionResult,
} from "./actions";

/**
 * How long a retry waits for a newer authoritative topology to arrive.
 *
 * This is a ceiling on how long a create can appear to hang, and with two
 * retries it used to be about four and a half seconds — the amplifier that
 * turned an ordinary `stale_topology` rejection into the user's "creates take
 * seconds". The host now answers a batched discovery in tens of milliseconds,
 * so a newer generation that is coming at all arrives well inside this bound;
 * anything slower is a stall the user is better off seeing than waiting through.
 */
const ACTION_RECONCILE_TIMEOUT_MS = 250;
const ACTION_RECONCILE_RETRIES = 2;

type ActionRequest = (
  clientId: string,
  action: TmuxAction,
  precondition: AuthoritativePrecondition,
) => Promise<TmuxActionResult>;

type ScopeWaiter = (
  attempted: HostScopeToken,
  current: () => HostScopeToken,
) => Promise<HostScopeToken | undefined>;

export interface ReconciledTmuxActionOptions {
  clientId: string;
  action: TmuxAction;
  initialScope: HostScopeToken;
  currentScope: () => HostScopeToken;
  capturedPrecondition?: AuthoritativePrecondition;
  request?: ActionRequest;
  waitForNewerScope?: ScopeWaiter;
}

export class TmuxActionScopeChangedError extends Error {
  constructor() {
    super("authoritative connection changed while the tmux action was in flight");
    this.name = "TmuxActionScopeChangedError";
  }
}

async function waitForNewerActionScope(
  attempted: HostScopeToken,
  current: () => HostScopeToken,
): Promise<HostScopeToken | undefined> {
  const deadline = Date.now() + ACTION_RECONCILE_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const candidate = current();
    if (!sameHostConnection(attempted, candidate)) return undefined;
    if (candidate.serverIdentity && candidate.generation > attempted.generation) return candidate;
    await new Promise((resolve) => globalThis.setTimeout(resolve, 16));
  }
  return undefined;
}

/**
 * Retries only non-destructive actions whose implicit topology generation went
 * stale while the user was interacting. Confirmed destructive actions and
 * callers with an explicit captured precondition remain exactly-once.
 */
export async function requestReconciledTmuxAction({
  clientId,
  action,
  initialScope,
  currentScope,
  capturedPrecondition,
  request = requestTmuxAction,
  waitForNewerScope = waitForNewerActionScope,
}: ReconciledTmuxActionOptions): Promise<TmuxActionResult> {
  if (!initialScope.serverIdentity) throw new Error("authoritative server identity is unavailable");
  let precondition = capturedPrecondition ?? {
    serverIdentity: initialScope.serverIdentity,
    generation: initialScope.generation,
  };
  let attemptedScope: HostScopeToken = {
    ...initialScope,
    serverIdentity: precondition.serverIdentity,
    generation: precondition.generation,
  };

  for (let retry = 0; ; retry += 1) {
    try {
      const result = await request(clientId, action, precondition);
      if (!sameHostConnection(attemptedScope, currentScope())) throw new TmuxActionScopeChangedError();
      return result;
    } catch (error) {
      const mayRetry = retry < ACTION_RECONCILE_RETRIES
        && !capturedPrecondition
        && !isDestructiveTmuxAction(action)
        && isStaleTmuxTopologyError(error);
      if (!mayRetry) throw error;
      const refreshed = await waitForNewerScope(attemptedScope, currentScope);
      if (!refreshed?.serverIdentity) throw error;
      attemptedScope = refreshed;
      precondition = {
        serverIdentity: refreshed.serverIdentity,
        generation: refreshed.generation,
      };
    }
  }
}

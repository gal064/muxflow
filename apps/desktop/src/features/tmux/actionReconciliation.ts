import { sameHostConnection, type HostScopeToken } from "../shell/hostScope";
import {
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
 * seconds". The host answers a batched discovery in tens of milliseconds, so
 * on a nearby host a newer generation that is coming at all arrives well
 * inside this bound.
 *
 * The bound is a second rather than the 250 ms that once sufficed because the
 * topology still has to cross the link: on a 300 ms round trip (2026-08-29,
 * shaped to 150 ms each way) it never made 250 ms, both retries expired, and
 * an ordinary refusal reached the user as an error. What reached the user that
 * way was a *switch*, and the host no longer refuses one for a stale
 * generation at all. What is left here is every kind that still carries the
 * guard — create, close, rename, reorder, split, resize, zoom, pin — for which
 * a slow link is exactly the case that needs the longer wait; this returns the
 * moment the newer topology lands, so a fast link pays nothing for it. Two
 * seconds of apparent hang is the worst case; anything slower than this is a
 * stall the user is better off seeing than waiting through.
 */
const ACTION_RECONCILE_TIMEOUT_MS = 1_000;
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
 * Retries an action whose topology generation went stale while the user was
 * interacting, against the newer authoritative generation.
 *
 * The one thing never retried is an action whose caller pinned a generation:
 * that stamp is a consent snapshot — the topology a confirmation dialog showed
 * the person — and re-issuing against a newer one would quietly act on a
 * different tmux than the one they agreed to. A caller that stamps only the
 * server identity (`generation: 0`, the host's "no generation guard") has
 * pinned nothing, so it reconciles like the implicit stamp does.
 *
 * Destructiveness no longer blocks a retry. Every refusal
 * `isStaleTmuxTopologyError` classifies is raised before the action's tmux
 * command runs: the host's three `stale topology` bails sit above the mutation
 * match in `service/tmux_actions.rs`, and the dispatcher's reconciliation
 * refusal in `requests/tmux_action_dispatch.rs` returns before `execute` is
 * even spawned. Nothing was executed, so a retry cannot double-execute — and
 * every failure that *can* follow a mutation is reported as `outcome unknown`,
 * which this deliberately does not match. Revisit if the host ever grows a
 * stale-topology bail after execution.
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
  /** A caller-pinned generation is consent; only that makes an action exactly-once. */
  const pinsGeneration = Boolean(capturedPrecondition) && precondition.generation !== 0;
  let attemptedScope: HostScopeToken = {
    ...initialScope,
    serverIdentity: precondition.serverIdentity,
    // The live generation, not the stamped one: an identity-only precondition
    // stamps 0, and waiting for "newer than 0" would return the current scope
    // immediately and re-issue with nothing reconciled.
    generation: precondition.generation || initialScope.generation,
  };

  for (let retry = 0; ; retry += 1) {
    try {
      const result = await request(clientId, action, precondition);
      if (!sameHostConnection(attemptedScope, currentScope())) throw new TmuxActionScopeChangedError();
      return result;
    } catch (error) {
      const mayRetry = retry < ACTION_RECONCILE_RETRIES
        && !pinsGeneration
        && isStaleTmuxTopologyError(error);
      if (!mayRetry) throw error;
      const refreshed = await waitForNewerScope(attemptedScope, currentScope);
      if (!refreshed?.serverIdentity) throw error;
      attemptedScope = refreshed;
      precondition = {
        serverIdentity: refreshed.serverIdentity,
        // The caller's shape survives the retry: one that guarded nothing but
        // the server keeps guarding nothing but the server, rather than
        // acquiring a generation guard it never asked for.
        generation: precondition.generation === 0 ? 0 : refreshed.generation,
      };
    }
  }
}

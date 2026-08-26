// The subscriber that turns agent transitions into notifications (design doc
// §13). It owns the two pieces of process-local state the decision rule needs —
// what has already been notified (step 5) and what is currently on screen —
// and it is the only place that posts or cancels.
//
// Every dependency is injected so the whole thing runs under vitest with a fake
// host: nothing here imports expo-notifications or react-native.

import { decideAgentNotification, type NotificationEvent } from "./decide";
import type { NotificationHost } from "./host";
import { encodePayload } from "./payload";
import { agentWorkspaceName, needsAttention } from "../../store/selectors";
import type { Agent, AgentTransition, SessionState } from "../../store/sessionStore";

export interface AgentNotifierDeps {
  host: NotificationHost;
  getState: () => SessionState;
  /** Store subscription, for the cancel sweep (§13, last paragraph). */
  subscribe: (listener: () => void) => () => void;
  onAgentTransition: (listener: (transition: AgentTransition) => void) => () => void;
  /** §13 step 6. */
  appInForeground: () => boolean;
  log?: ((line: string) => void) | undefined;
}

export interface AgentNotifier {
  start(): void;
  stop(): void;
  /** Resolves once every post/cancel queued so far has settled. Tests await it. */
  settled(): Promise<void>;
  /** Tags currently on screen, for tests and diagnostics. */
  outstandingTags(): string[];
}

/** What a posted notification was posted *for*, so the sweep knows when it is stale. */
interface Outstanding {
  agentId: string;
  event: NotificationEvent;
  attentionGeneration: bigint;
  /** Whether the agent still wanted attention when this was posted (see `stale`). */
  unseenWhenPosted: boolean;
}

export function createAgentNotifier(deps: AgentNotifierDeps): AgentNotifier {
  /**
   * Step 5. Generations are monotonic per agent, so the highest generation
   * posted for an agent answers "was this pair already notified" in O(1) — and
   * suppresses a replayed *older* generation too, which set membership alone
   * would let through.
   */
  const lastNotified = new Map<string, bigint>();
  const outstanding = new Map<string, Outstanding>();
  const unsubscribes: (() => void)[] = [];
  // Posts and cancels are serialised so a cancel can never overtake the post it
  // is meant to undo, and so tests have a single thing to await.
  let queue: Promise<void> = Promise.resolve();
  let running = false;

  const log = (line: string): void => deps.log?.(`notifications: ${line}`);

  const enqueue = (work: () => Promise<void>): void => {
    queue = queue.then(work).catch((error: unknown) => {
      log(`failed ${error instanceof Error ? error.message : String(error)}`);
    });
  };

  function onTransition(transition: AgentTransition): void {
    const state = deps.getState();
    const decision = decideAgentNotification(transition.prev, transition.next, {
      notificationWatermark: state.notificationWatermark,
      alreadyNotified: (agentId, generation) => (lastNotified.get(agentId) ?? -1n) >= generation,
      focusedPaneId: state.focusedPaneId,
      appInForeground: deps.appInForeground(),
      workspaceName: agentWorkspaceName(state, transition.next),
    });
    if (decision.kind === "skip") {
      log(`skip ${transition.next.id} ${decision.reason}`);
      return;
    }
    const next = transition.next;
    const previouslyNotified = lastNotified.get(next.id);
    lastNotified.set(next.id, next.attentionGeneration);
    outstanding.set(decision.tag, {
      agentId: next.id,
      event: decision.event,
      attentionGeneration: next.attentionGeneration,
      unseenWhenPosted: needsAttention(next),
    });
    log(`post ${next.id} ${decision.event} gen=${next.attentionGeneration}`);
    enqueue(async () => {
      try {
        await deps.host.present({
          tag: decision.tag,
          title: decision.title,
          body: decision.body,
          data: encodePayload(decision.data),
        });
      } catch (error) {
        // Nothing was shown, so nothing is outstanding and this generation was
        // not notified: undo the bookkeeping rather than swallow the event for
        // the rest of the process.
        if (outstanding.get(decision.tag)?.attentionGeneration === next.attentionGeneration) {
          outstanding.delete(decision.tag);
        }
        if (lastNotified.get(next.id) === next.attentionGeneration) {
          if (previouslyNotified === undefined) lastNotified.delete(next.id);
          else lastNotified.set(next.id, previouslyNotified);
        }
        throw error;
      }
    });
  }

  /** §13: cancel by tag once the notification no longer stands for anything. */
  function sweep(): void {
    if (outstanding.size === 0) return;
    const state = deps.getState();
    // Only a live host can say that an agent is gone. `HostConnection` clears
    // the whole agent map on every Subscribe — including a routine reconnect —
    // before the snapshot refills it, and a disconnect empties it too; reading
    // either as "retired" would take an unread "Needs your input" off the lock
    // screen, which §12 explicitly does not want and step 4 would then stop
    // from ever being re-posted.
    if (state.connection.state !== "connected") return;
    const agents = state.agents;
    for (const [tag, entry] of [...outstanding]) {
      if (!stale(entry, agents[entry.agentId])) continue;
      outstanding.delete(tag);
      log(`cancel ${tag}`);
      enqueue(() => deps.host.cancel(tag));
    }
  }

  return {
    start() {
      if (running) return;
      running = true;
      unsubscribes.push(deps.onAgentTransition(onTransition), deps.subscribe(sweep));
    },
    stop() {
      running = false;
      for (const unsubscribe of unsubscribes.splice(0)) unsubscribe();
    },
    settled() {
      // Two hops: work enqueued by the current tail may itself have been queued
      // while this promise was being built.
      return queue.then(() => queue);
    },
    outstandingTags() {
      return [...outstanding.keys()];
    },
  };
}

/**
 * §13's "when an agent's `needsAttention` becomes false for any reason (seen on
 * desktop, retired), cancel its notification by tag", made precise:
 *
 * - the agent is gone from the map or no longer present → retired;
 * - it wanted attention when the notification went out and no longer does →
 *   marked seen, here or on the desktop. The guard matters because §13 step 3's
 *   second branch also notifies a fresh `blocked` lifecycle with no attention
 *   advance behind it, and such a notification must not cancel itself the
 *   instant it is posted;
 * - a `blocked` notification whose agent has left `blocked` → it moved on. A
 *   `completed` one is not cancelled this way: `completed` *is* the idle state
 *   it reports, and it stands until seen.
 */
function stale(entry: Outstanding, agent: Agent | undefined): boolean {
  if (agent === undefined || !agent.present) return true;
  if (entry.unseenWhenPosted && !needsAttention(agent)) return true;
  return entry.event === "blocked" && agent.lifecycle !== "blocked";
}

import { useEffect, useState } from "react";
import { recentAgentExpiration } from "./agentsList";
import type { AgentRecord } from "./types";

/**
 * Re-renders the priority list exactly when its next Recent row becomes Idle.
 * Agent events already cause their own renders; this clock owns only the one
 * transition that can happen with no event at all.
 */
export function useRecentIdleClock(agents: readonly AgentRecord[], enabled: boolean): number {
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    if (!enabled) return;
    const now = Date.now();
    let nextExpiration: number | undefined;
    for (const agent of agents) {
      const expiration = recentAgentExpiration(agent);
      if (expiration === undefined || expiration <= now) continue;
      nextExpiration = nextExpiration === undefined
        ? expiration
        : Math.min(nextExpiration, expiration);
    }
    if (nextExpiration === undefined) return;
    const timer = globalThis.setTimeout(
      () => setRevision((current) => current + 1),
      nextExpiration - now,
    );
    return () => globalThis.clearTimeout(timer);
  }, [agents, enabled, revision]);

  return revision;
}

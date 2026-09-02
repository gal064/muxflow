import { useEffect, useState } from "react";

import type { Agent } from "../../store/sessionStore";
import { nextRecentExpiration } from "./agentListModel";

/**
 * Re-renders the priority list exactly when its next Recent row becomes Idle
 * (the desktop's hook of the same name). Agent events already cause their own
 * renders; this clock owns only the one transition that can happen with no
 * event at all. Returns a revision to fold into the list's `now`.
 */
export function useRecentIdleClock(agents: readonly Agent[], enabled: boolean): number {
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    const now = Date.now();
    const next = nextRecentExpiration(agents, now);
    if (next === undefined) return;
    const timer = setTimeout(() => setRevision((current) => current + 1), next - now);
    return () => clearTimeout(timer);
  }, [agents, enabled, revision]);
  return revision;
}

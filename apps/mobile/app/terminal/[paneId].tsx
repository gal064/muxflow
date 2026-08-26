import { useLocalSearchParams } from "expo-router";

import { TerminalScreen } from "../../src/features/terminal/TerminalScreen";
import { useSession } from "../../src/ui/hooks";

/** Terminal — design.md §9.5. The one screen without the standard app bar. */
export default function TerminalRoute() {
  const { paneId, sessionId } = useLocalSearchParams<{ paneId: string; sessionId?: string }>();
  const fromTopology = useSession((s) => (paneId ? s.panes[paneId]?.sessionId : undefined));
  const resolvedSession = sessionId || fromTopology || "";
  if (!paneId) return null;
  return <TerminalScreen key={`${paneId}:${resolvedSession}`} paneId={paneId} sessionId={resolvedSession} />;
}

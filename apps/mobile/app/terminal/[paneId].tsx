import { useLocalSearchParams } from "expo-router";

import { TerminalScreen } from "../../src/features/terminal/TerminalScreen";
import { fromRouteParam } from "../../src/navigation/routeParams";
import { useSession } from "../../src/ui/hooks";

/** Terminal — design.md §9.5. The one screen without the standard app bar. */
export default function TerminalRoute() {
  const params = useLocalSearchParams<{ paneId: string; sessionId?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const sessionId = fromRouteParam(params.sessionId);
  const fromTopology = useSession((s) => (paneId ? s.panes[paneId]?.sessionId : undefined));
  const resolvedSession = sessionId || fromTopology || "";
  if (!paneId) return null;
  return <TerminalScreen key={`${paneId}:${resolvedSession}`} paneId={paneId} sessionId={resolvedSession} />;
}

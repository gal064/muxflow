import { useLocalSearchParams } from "expo-router";

import { TerminalScreen } from "../../src/features/terminal/TerminalScreen";
import { fromRouteParam } from "../../src/navigation/routeParams";
import { useSession } from "../../src/ui/hooks";

/** Terminal — design.md §9.5. The one screen without the standard app bar. */
export default function TerminalRoute() {
  const params = useLocalSearchParams<{ paneId: string; sessionId?: string; createdGeneration?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const sessionId = fromRouteParam(params.sessionId);
  const createdGeneration = parseGeneration(params.createdGeneration);
  const fromTopology = useSession((s) => (paneId ? s.panes[paneId]?.sessionId : undefined));
  const resolvedSession = sessionId || fromTopology || "";
  if (!paneId) return null;
  return <TerminalScreen createdGeneration={createdGeneration} key={`${paneId}:${resolvedSession}`} paneId={paneId} sessionId={resolvedSession} />;
}

function parseGeneration(value: string | undefined): bigint | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined;
  const generation = BigInt(value);
  return generation > 0n ? generation : undefined;
}

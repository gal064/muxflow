import { useLocalSearchParams } from "expo-router";

import { VoiceScreen } from "../../src/features/voice/VoiceScreen";
import { fromRouteParam } from "../../src/navigation/routeParams";
import { useSession } from "../../src/ui/hooks";

/** Voice — design.md §9.11. Header of its own, like the Terminal screen. */
export default function VoiceRoute() {
  const params = useLocalSearchParams<{ paneId: string; sessionId?: string; agentId?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const sessionId = fromRouteParam(params.sessionId);
  const agentId = fromRouteParam(params.agentId);
  const fromTopology = useSession((s) => (paneId ? s.panes[paneId]?.sessionId : undefined));
  const resolvedSession = sessionId || fromTopology || "";
  if (!paneId || !agentId) return null;
  return <VoiceScreen key={`${agentId}:${paneId}`} agentId={agentId} paneId={paneId} sessionId={resolvedSession} />;
}

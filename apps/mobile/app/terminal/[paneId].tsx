import { Redirect, useLocalSearchParams } from "expo-router";

import { TerminalScreen } from "../../src/features/terminal/TerminalScreen";
import { fromRouteParam, toRouteParam } from "../../src/navigation/routeParams";
import { useSession } from "../../src/ui/hooks";

/** Terminal — design.md §9.5. The one screen without the standard app bar. A chat pane opens as its chat. */
export default function TerminalRoute() {
  const params = useLocalSearchParams<{ paneId: string; sessionId?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const sessionId = fromRouteParam(params.sessionId);
  const fromTopology = useSession((s) => (paneId ? s.panes[paneId]?.sessionId : undefined));
  const chat = useSession((s) => (paneId ? (s.panes[paneId]?.chatAdapterId ?? "") !== "" : false));
  const resolvedSession = sessionId || fromTopology || "";
  if (!paneId) return null;
  if (chat) return <Redirect href={{ pathname: "/chat/[paneId]", params: { paneId: toRouteParam(paneId), sessionId: toRouteParam(resolvedSession) } }} />;
  return <TerminalScreen key={`${paneId}:${resolvedSession}`} paneId={paneId} sessionId={resolvedSession} />;
}

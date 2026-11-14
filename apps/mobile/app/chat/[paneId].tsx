import { useLocalSearchParams } from "expo-router";

import { ChatScreen } from "../../src/features/chat/ChatScreen";
import { fromRouteParam } from "../../src/navigation/routeParams";
import { useSession } from "../../src/ui/hooks";

/** Chat — a chat pane (docs/design/chat-plan.md §5 F). Header of its own, like the Terminal screen. */
export default function ChatRoute() {
  const params = useLocalSearchParams<{ paneId: string; sessionId?: string; voice?: string }>();
  const paneId = fromRouteParam(params.paneId);
  const sessionId = fromRouteParam(params.sessionId);
  const fromTopology = useSession((s) => (paneId ? s.panes[paneId]?.sessionId : undefined));
  if (!paneId) return null;
  return <ChatScreen key={paneId} paneId={paneId} sessionId={sessionId || fromTopology || ""} startInVoice={params.voice === "1"} />;
}

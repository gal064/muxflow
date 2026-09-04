import { useRouter } from "expo-router";
import { useCallback, useState } from "react";

import { agentTitle, noAdapterWired } from "../../src/features/agents/agentViews";
import { markSeenIfNeeded } from "../../src/features/agents/markSeen";
import { refreshAgents } from "../../src/features/agents/refresh";
import { AgentList } from "../../src/features/agents/ui/AgentList";
import { toRouteParam } from "../../src/navigation/routeParams";
import { toast } from "../../src/session/connectionManager";
import type { Agent } from "../../src/store/sessionStore";
import { EmptyState } from "../../src/ui/components/EmptyState";
import { useSession } from "../../src/ui/hooks";
import { WorkspaceActionsSheet } from "../../src/features/terminal/WorkspaceActionsSheet";

/** Agents tab — design.md §9.3.1. The list itself lives in `AgentList`. */
export default function AgentsScreen() {
  const router = useRouter();
  const connected = useSession((s) => s.connection.state === "connected");
  const adapters = useSession((s) => s.adapters);
  const windows = useSession((s) => s.windows);
  const [refreshing, setRefreshing] = useState(false);
  const [actionsFor, setActionsFor] = useState<Agent | null>(null);
  const onRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await refreshAgents();
    } finally {
      setRefreshing(false);
    }
  }, []);
  const open = useCallback((agent: Agent) => {
    // A retained (gone) agent may have no pane; "/terminal/" would be an unmatched route.
    if (!agent.route.paneId) {
      toast("This agent has no terminal.");
      return;
    }
    markSeenIfNeeded(agent);
    router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(agent.route.paneId), sessionId: toRouteParam(agent.route.sessionId) } });
  }, [router]);
  const talk = useCallback((agent: Agent) => {
    if (!agent.route.paneId) {
      toast("This agent has no terminal.");
      return;
    }
    markSeenIfNeeded(agent);
    router.push({ pathname: "/voice/[paneId]", params: { paneId: toRouteParam(agent.route.paneId), sessionId: toRouteParam(agent.route.sessionId), agentId: toRouteParam(agent.id) } });
  }, [router]);

  const empty = (
    <EmptyState
      heading="No agents running"
      lines={[
        "Agents started from the desktop show up here. Codex and Claude Code are supported.",
        ...(noAdapterWired(adapters)
          ? ["Agent status hooks are not set up on this host. Set them up from the desktop: Settings → Connection → Set up agent status…"]
          : []),
      ]}
    />
  );

  return (
    <>
      <AgentList empty={connected ? empty : null} onLongPress={setActionsFor} onOpen={open} onRefresh={onRefresh} onTalk={talk} refreshing={refreshing} />
      <WorkspaceActionsSheet
        onDismiss={() => setActionsFor(null)}
        sessionId={actionsFor?.route.sessionId}
        title={actionsFor ? agentTitle({ adapters, windows }, actionsFor) : undefined}
        visible={actionsFor !== null}
      />
    </>
  );
}

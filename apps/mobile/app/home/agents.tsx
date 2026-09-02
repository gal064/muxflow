import { useRouter } from "expo-router";
import { useCallback, useState } from "react";

import { noAdapterWired } from "../../src/features/agents/agentViews";
import { markSeenIfNeeded } from "../../src/features/agents/markSeen";
import { refreshAgents } from "../../src/features/agents/refresh";
import { AgentList } from "../../src/features/agents/ui/AgentList";
import { toast } from "../../src/session/connectionManager";
import type { Agent } from "../../src/store/sessionStore";
import { EmptyState } from "../../src/ui/components/EmptyState";
import { useSession } from "../../src/ui/hooks";

/** Agents tab — design.md §9.3.1. The list itself lives in `AgentList`. */
export default function AgentsScreen() {
  const router = useRouter();
  const connected = useSession((s) => s.connection.state === "connected");
  const adapters = useSession((s) => s.adapters);
  const [refreshing, setRefreshing] = useState(false);
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
    router.push({ pathname: "/terminal/[paneId]", params: { paneId: agent.route.paneId, sessionId: agent.route.sessionId } });
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

  return <AgentList empty={connected ? empty : null} onOpen={open} onRefresh={onRefresh} refreshing={refreshing} />;
}

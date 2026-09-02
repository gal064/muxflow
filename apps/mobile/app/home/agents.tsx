import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { FlatList, RefreshControl, StyleSheet, Text, View } from "react-native";

import { agentPillState, agentTitle, noAdapterWired } from "../../src/features/agents/agentViews";
import { markSeenIfNeeded } from "../../src/features/agents/markSeen";
import { refreshAgents } from "../../src/features/agents/refresh";
import { NotificationsOffBanner } from "../../src/features/notifications/ui/NotificationsOffBanner";
import { toRouteParam } from "../../src/navigation/routeParams";
import { toast } from "../../src/session/connectionManager";
import { agentWindowName, agentWorkspaceName, displayState, needsAttention, sortedAgents } from "../../src/store/selectors";
import type { Agent } from "../../src/store/sessionStore";
import { EmptyState } from "../../src/ui/components/EmptyState";
import { ListRow } from "../../src/ui/components/ListRow";
import { StatusPill } from "../../src/ui/components/StatusPill";
import { useSession } from "../../src/ui/hooks";
import { colors, metrics, radii, terminalTheme } from "../../src/ui/tokens";

/** Agents tab — design.md §9.3.1. */
export default function AgentsScreen() {
  const router = useRouter();
  const state = useSession((s) => s);
  const agents = sortedAgents(state);
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
    router.push({ pathname: "/terminal/[paneId]", params: { paneId: toRouteParam(agent.route.paneId), sessionId: toRouteParam(agent.route.sessionId) } });
  }, [router]);

  const empty = (
    <EmptyState
      heading="No agents running"
      lines={[
        "Agents started from the desktop show up here. Codex and Claude Code are supported.",
        ...(noAdapterWired(state.adapters)
          ? ["Agent status hooks are not set up on this host. Set them up from the desktop: Settings → Connection → Set up agent status…"]
          : []),
      ]}
    />
  );

  return (
    <FlatList
      contentContainerStyle={agents.length === 0 ? styles.fill : undefined}
      data={agents}
      keyExtractor={(agent) => agent.id}
      ListEmptyComponent={state.connection.state === "connected" ? empty : null}
      ListHeaderComponent={NotificationsOffBanner}
      refreshControl={<RefreshControl colors={[colors.accent]} progressBackgroundColor={colors.chromeRaised} onRefresh={onRefresh} refreshing={refreshing} />}
      renderItem={({ item }) => {
        const attention = needsAttention(item);
        const shown = displayState(item);
        return (
          <>
          <ListRow
            dimmed={!item.present}
            edgeColor={attention ? (shown === "done" ? terminalTheme.green : colors.danger) : undefined}
            height={metrics.agentRowHeight}
            leading={<AdapterAvatar adapterId={item.adapterId} />}
            onPress={() => open(item)}
            subtitle={`${agentWorkspaceName(state, item)} · ${agentWindowName(state, item)}`}
            title={agentTitle(state, item)}
            trailing={<StatusPill state={agentPillState(item)} />}
          />
          </>
        );
      }}
      style={styles.list}
    />
  );
}

/** 36 dp rounded square: Codex `⌘` in `--term-5`, Claude Code `✱` in `--term-3` (§9.3.1). */
function AdapterAvatar({ adapterId }: { adapterId: string }) {
  const codex = adapterId === "codex";
  return (
    <View style={styles.avatar}>
      <Text style={[styles.avatarGlyph, { color: codex ? terminalTheme.magenta : terminalTheme.yellow }]}>{codex ? "⌘" : "✱"}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  list: { backgroundColor: colors.chromeBg, flex: 1 },
  fill: { flexGrow: 1 },
  avatar: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.card,
    height: metrics.agentAvatarSize,
    justifyContent: "center",
    width: metrics.agentAvatarSize,
  },
  avatarGlyph: { fontSize: 18, fontWeight: "700" },
});

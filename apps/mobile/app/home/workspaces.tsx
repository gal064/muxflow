import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { FlatList, RefreshControl, StyleSheet, Text } from "react-native";

import { attentionCountInSession, windowCountLabel } from "../../src/features/agents/agentViews";
import { refreshAgents } from "../../src/features/agents/refresh";
import { stripAgentStatusGlyphs } from "../../src/features/agents/agentLabels";
import { EmptyState } from "../../src/ui/components/EmptyState";
import { ListRow } from "../../src/ui/components/ListRow";
import { useSession } from "../../src/ui/hooks";
import { colors, metrics, typeScale } from "../../src/ui/tokens";

/** Workspaces tab — design.md §9.3.2. */
export default function WorkspacesScreen() {
  const router = useRouter();
  const state = useSession((s) => s);
  const sessions = Object.values(state.sessions).sort((a, b) => a.order - b.order);
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await refreshAgents();
    } finally {
      setRefreshing(false);
    }
  }, []);

  return (
    <FlatList
      contentContainerStyle={sessions.length === 0 ? styles.fill : undefined}
      data={sessions}
      keyExtractor={(session) => session.id}
      ListEmptyComponent={state.connection.state === "connected" ? <EmptyState heading="No workspaces" lines={["tmux has no sessions on this host."]} /> : null}
      refreshControl={<RefreshControl colors={[colors.accent]} progressBackgroundColor={colors.chromeRaised} onRefresh={onRefresh} refreshing={refreshing} />}
      renderItem={({ item }) => {
        const attention = attentionCountInSession(state, item.id);
        return (
          <ListRow
            edgeColor={attention > 0 ? colors.danger : undefined}
            height={metrics.sessionRowHeight}
            onPress={() => router.push({ pathname: "/workspace/[sessionId]", params: { sessionId: item.id } })}
            subtitle={windowCountLabel(item.windowCount)}
            title={stripAgentStatusGlyphs(item.name)}
            trailing={attention > 0 ? <Text style={styles.needYou}>{attention} need you</Text> : undefined}
          />
        );
      }}
      style={styles.list}
    />
  );
}

const styles = StyleSheet.create({
  list: { backgroundColor: colors.chromeBg, flex: 1 },
  fill: { flexGrow: 1 },
  needYou: { color: colors.danger, fontSize: typeScale.meta, fontWeight: "600" },
});

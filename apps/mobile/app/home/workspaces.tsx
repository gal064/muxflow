import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { FlatList, RefreshControl, StyleSheet, Text } from "react-native";

import { attentionCountInSession, windowCountLabel } from "../../src/features/agents/agentViews";
import { refreshAgents } from "../../src/features/agents/refresh";
import { stripAgentStatusGlyphs } from "../../src/features/agents/agentLabels";
import { EmptyState } from "../../src/ui/components/EmptyState";
import { ListDivider } from "../../src/ui/components/ListDivider";
import { ListRow } from "../../src/ui/components/ListRow";
import { pinnedDividers } from "../../src/store/selectors";
import { useSession } from "../../src/ui/hooks";
import { colors, metrics, typeScale } from "../../src/ui/tokens";

/** Workspaces tab — design.md §9.3.2. */
export default function WorkspacesScreen() {
  const router = useRouter();
  const state = useSession((s) => s);
  // Host order, with pinned workspaces lifted to a leading block (the desktop sidebar's rule).
  const ordered = Object.values(state.sessions).sort((a, b) => a.order - b.order);
  const sessions = [...ordered.filter((s) => s.pinned), ...ordered.filter((s) => !s.pinned)];
  const dividers = pinnedDividers(sessions.map((s) => s.pinned));
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
      renderItem={({ item, index }) => {
        const attention = attentionCountInSession(state, item.id);
        return (
          <>
          {index === dividers.pinnedAt ? <ListDivider label="Pinned" /> : null}
          {index === dividers.restAt ? <ListDivider label="Workspaces" /> : null}
          <ListRow
            edgeColor={attention > 0 ? colors.danger : undefined}
            height={metrics.sessionRowHeight}
            onPress={() => router.push({ pathname: "/workspace/[sessionId]", params: { sessionId: item.id } })}
            subtitle={windowCountLabel(item.windowCount)}
            title={stripAgentStatusGlyphs(item.name)}
            trailing={attention > 0 ? <Text style={styles.needYou}>{attention} need you</Text> : undefined}
          />
          </>
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

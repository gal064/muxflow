import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { FlatList, RefreshControl, StyleSheet, Text } from "react-native";

import { waitingColor, waitingInSession, waitingLabel, windowCountLabel } from "../../src/features/agents/agentViews";
import { refreshAgents } from "../../src/features/agents/refresh";
import { stripAgentStatusGlyphs } from "../../src/features/agents/agentLabels";
import { toRouteParam } from "../../src/navigation/routeParams";
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
        // The desktop's workspace row: the bar takes the loudest waiting
        // state's colour (blocked red over done green) and the chip counts
        // everyone waiting, the same predicate the Agents tab's bars use.
        const waiting = waitingInSession(state, item.id);
        return (
          <>
          {index === dividers.pinnedAt ? <ListDivider label="Pinned" /> : null}
          {index === dividers.restAt ? <ListDivider label="Workspaces" /> : null}
          <ListRow
            edgeColor={waiting.loudest ? waitingColor(waiting.loudest) : undefined}
            height={metrics.sessionRowHeight}
            onPress={() => router.push({ pathname: "/workspace/[sessionId]", params: { sessionId: toRouteParam(item.id) } })}
            subtitle={windowCountLabel(item.windowCount)}
            title={stripAgentStatusGlyphs(item.name)}
            trailing={waiting.loudest ? <Text style={[styles.waiting, { color: waitingColor(waiting.loudest) }]}>{waitingLabel(waiting.count)}</Text> : undefined}
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
  waiting: { fontSize: typeScale.meta, fontWeight: "600" },
});

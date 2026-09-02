import { useMemo, type ReactElement } from "react";
import { FlatList, Pressable, RefreshControl, StyleSheet, Text, View } from "react-native";
import { useStore } from "zustand";

import { prefsStore } from "../../../store/prefsStore";
import type { Agent } from "../../../store/sessionStore";
import { ListDivider } from "../../../ui/components/ListDivider";
import { ListRow } from "../../../ui/components/ListRow";
import { StatusPill } from "../../../ui/components/StatusPill";
import { useSession } from "../../../ui/hooks";
import { colors, fonts, metrics, radii, typeScale } from "../../../ui/tokens";
import { NotificationsOffBanner } from "../../notifications/ui/NotificationsOffBanner";
import { AGENT_LIST_MODES, buildAgentListItems, type AgentListItem, type AgentListMode } from "../agentListModel";
import { useRecentIdleClock } from "../useRecentIdleClock";
import { PinIcon } from "./AgentIcon";
import { AgentMark, StateBadge } from "./AgentMark";

interface AgentListProps {
  /** Owned by the screen so the navigation call stays in one place. */
  onOpen(agent: Agent): void;
  refreshing: boolean;
  onRefresh(): void;
  empty: ReactElement | null;
}

/** The Agents tab's list (design.md §9.3.1): the desktop's two orders, headed. */
export function AgentList({ onOpen, refreshing, onRefresh, empty }: AgentListProps) {
  const agents = useSession((s) => s.agents);
  const sessions = useSession((s) => s.sessions);
  const windows = useSession((s) => s.windows);
  const adapters = useSession((s) => s.adapters);
  const mode = useStore(prefsStore, (s) => s.agentListMode);
  const agentList = useMemo(() => Object.values(agents), [agents]);
  const revision = useRecentIdleClock(agentList, mode === "priority");
  const items = useMemo(
    () => buildAgentListItems({ agents, sessions, windows, adapters }, mode, Date.now()),
    // `revision` ticks when a Recent row ages out with no store update.
    [agents, sessions, windows, adapters, mode, revision],
  );

  return (
    <FlatList
      contentContainerStyle={items.length === 0 ? styles.fill : styles.content}
      data={items}
      keyExtractor={(item) => item.key}
      ListEmptyComponent={empty}
      ListHeaderComponent={
        <>
          <NotificationsOffBanner />
          {items.length > 0 ? <ModeToggle mode={mode} onChange={(next) => prefsStore.getState().setAgentListMode(next)} /> : null}
        </>
      }
      refreshControl={<RefreshControl colors={[colors.accent]} progressBackgroundColor={colors.chromeRaised} onRefresh={onRefresh} refreshing={refreshing} />}
      renderItem={({ item }) => <Item item={item} onOpen={onOpen} />}
      style={styles.list}
    />
  );
}

function Item({ item, onOpen }: { item: AgentListItem; onOpen(agent: Agent): void }) {
  switch (item.kind) {
    case "divider":
      return <ListDivider label={item.label} />;
    case "section":
      return (
        <GroupHeading count={item.count} label={item.label}>
          <StateBadge ring={colors.chromeBg} size={12} state={item.state} />
        </GroupHeading>
      );
    case "group":
      return (
        <GroupHeading count={item.count} label={item.workspaceName}>
          {item.pinned ? <PinIcon color={colors.chromeDim} size={12} /> : null}
        </GroupHeading>
      );
    case "agent":
      return (
        <ListRow
          dimmed={!item.agent.present}
          // Unread blocked or completed: the mobile shape of the desktop's
          // "1" badge. The docked dot says blocked; the bar says *unread*.
          edgeColor={item.attention ? (item.state === "done" ? colors.ok : colors.danger) : undefined}
          height={metrics.agentRowHeight}
          leading={<AgentMark adapterId={item.agent.adapterId} state={item.state} />}
          onPress={() => onOpen(item.agent)}
          subtitle={item.subtitle}
          title={item.title}
          titleAccessory={item.pinned ? <PinIcon color={colors.chromeDim} size={13} /> : null}
          // A gone agent has no live state to dock; the word is the only honest mark.
          trailing={item.agent.present ? undefined : <StatusPill state="gone" />}
        />
      );
  }
}

/**
 * A group's heading: an optional mark, the label, and the count out at the
 * right edge — the desktop's `.agent-workspace-heading`. The count is what
 * tells you a collapsed-looking group has six agents in it.
 */
function GroupHeading({ label, count, children }: { label: string; count: number; children?: React.ReactNode }) {
  return (
    <View style={styles.heading}>
      {children ? <View style={styles.headingMark}>{children}</View> : null}
      <Text numberOfLines={1} style={styles.headingLabel}>{label}</Text>
      <Text style={styles.headingCount}>{count}</Text>
    </View>
  );
}

/** Priority | Workspace — the desktop's sort toggle, as a segmented control. */
function ModeToggle({ mode, onChange }: { mode: AgentListMode; onChange(mode: AgentListMode): void }) {
  return (
    <View style={styles.toggleRow}>
      <View accessibilityRole="tablist" style={styles.toggle}>
        {AGENT_LIST_MODES.map((entry) => {
          const selected = entry.mode === mode;
          return (
            <Pressable
              accessibilityRole="tab"
              accessibilityState={{ selected }}
              key={entry.mode}
              onPress={() => onChange(entry.mode)}
              style={[styles.segment, selected && styles.segmentSelected]}
            >
              <Text style={[styles.segmentLabel, selected && styles.segmentLabelSelected]}>{entry.label}</Text>
            </Pressable>
          );
        })}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  list: { backgroundColor: colors.chromeBg, flex: 1 },
  fill: { flexGrow: 1 },
  content: { paddingBottom: 24 },
  heading: {
    alignItems: "center",
    flexDirection: "row",
    gap: 8,
    paddingBottom: 6,
    paddingHorizontal: 16,
    paddingTop: 16,
  },
  headingMark: { alignItems: "center", justifyContent: "center", width: 14 },
  headingLabel: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  headingCount: {
    color: colors.chromeFaint,
    fontFamily: fonts.mono,
    fontSize: typeScale.meta,
    fontVariant: ["tabular-nums"],
    marginLeft: "auto",
  },
  toggleRow: { alignItems: "flex-end", paddingHorizontal: 16, paddingTop: 12 },
  toggle: {
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.pill,
    flexDirection: "row",
    padding: 2,
  },
  segment: { borderRadius: radii.pill - 2, minHeight: 32, justifyContent: "center", paddingHorizontal: 14 },
  segmentSelected: { backgroundColor: colors.chromeSelected },
  segmentLabel: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  segmentLabelSelected: { color: colors.chromeInkStrong },
});

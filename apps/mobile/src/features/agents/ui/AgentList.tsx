import { useMemo, type ReactElement } from "react";
import { FlatList, Pressable, RefreshControl, StyleSheet, Text, View } from "react-native";
import { useStore } from "zustand";

import { prefsStore } from "../../../store/prefsStore";
import type { AgentDisplayState } from "../../../store/selectors";
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
      renderItem={({ item, index }) => <Item afterDivider={items[index - 1]?.kind === "divider"} item={item} onOpen={onOpen} />}
      style={styles.list}
    />
  );
}

function Item({ item, onOpen, afterDivider }: { item: AgentListItem; onOpen(agent: Agent): void; afterDivider: boolean }) {
  switch (item.kind) {
    case "divider":
      return <ListDivider label={item.label} />;
    case "section":
      return (
        <GroupHeading afterDivider={afterDivider} count={item.count} label={item.label}>
          <StateBadge ink={colors.chromeDim} ring={colors.chromeBg} size={12} state={item.state} />
        </GroupHeading>
      );
    case "group":
      return (
        <GroupHeading afterDivider={afterDivider} count={item.count} label={item.workspaceName}>
          {item.pinned ? <PinIcon color={colors.chromeDim} size={12} /> : null}
        </GroupHeading>
      );
    case "agent":
      return (
        <ListRow
          // The desktop's aria-label: the state is drawn, so it is spoken here.
          accessibilityLabel={[item.title, item.agent.present ? STATE_WORDS[item.state] : "gone", item.attention ? "needs you" : undefined, item.subtitle].filter(Boolean).join(", ")}
          dimmed={!item.agent.present}
          // Unread blocked or completed: the mobile shape of the desktop's
          // "1" badge. The docked dot says blocked; the bar says *unread*.
          edgeColor={item.attention ? (item.state === "done" ? colors.ok : colors.danger) : undefined}
          height={metrics.agentRowHeight}
          leading={<AgentMark adapterId={item.agent.adapterId} state={item.state} />}
          onPress={() => onOpen(item.agent)}
          subtitle={item.subtitle}
          title={item.title}
          titleAccessory={item.pinned ? <PinIcon color={colors.chromeDim} size={14} /> : null}
          // A gone agent has no live state to dock; the word is the only honest mark.
          trailing={item.agent.present ? undefined : <StatusPill state="gone" />}
        />
      );
  }
}

const STATE_WORDS: Record<AgentDisplayState, string> = {
  blocked: "blocked",
  working: "working",
  done: "finished",
  idle: "idle",
  unknown: "status unknown",
};

/**
 * A group's heading: a mark slot, the label, and the count out at the right
 * edge — the desktop's `.agent-workspace-heading`. The mark slot is the width
 * of the rows' avatar column whether or not a mark is drawn, so heading
 * labels align with row titles and a heading reads as a different level from
 * the full-bleed Pinned / Others dividers. The count is what tells you a
 * collapsed-looking group has six agents in it.
 */
function GroupHeading({ label, count, children, afterDivider }: { label: string; count: number; children?: React.ReactNode; afterDivider: boolean }) {
  return (
    <View style={[styles.heading, afterDivider && styles.headingAfterDivider]}>
      <View style={styles.headingMark}>{children}</View>
      <Text numberOfLines={1} style={styles.headingLabel}>{label}</Text>
      <Text style={styles.headingCount}>{count}</Text>
    </View>
  );
}

/**
 * Priority | Workspace — the desktop's sort toggle, as a segmented control.
 *
 * Each segment's pressable is the full 48 dp touch target and the 32 dp pill
 * sits inside it; the track is painted behind the row rather than wrapping
 * it, because a hit slop never extends past the parent's bounds and a track
 * that wrapped the pills would have capped the target at their height.
 */
function ModeToggle({ mode, onChange }: { mode: AgentListMode; onChange(mode: AgentListMode): void }) {
  return (
    <View style={styles.toggleRow}>
      <View accessibilityRole="tablist" style={styles.toggle}>
        <View pointerEvents="none" style={styles.toggleTrack} />
        {AGENT_LIST_MODES.map((entry) => {
          const selected = entry.mode === mode;
          return (
            <Pressable
              accessibilityRole="tab"
              accessibilityState={{ selected }}
              key={entry.mode}
              onPress={() => onChange(entry.mode)}
              style={styles.segmentTarget}
            >
              <View style={[styles.segment, selected && styles.segmentSelected]}>
                <Text style={[styles.segmentLabel, selected && styles.segmentLabelSelected]}>{entry.label}</Text>
              </View>
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
    gap: 12,
    paddingBottom: 6,
    paddingHorizontal: 16,
    paddingTop: 16,
  },
  // A divider already supplies the space above (the desktop's `.list-block:first-child > .list-divider`).
  headingAfterDivider: { paddingTop: 6 },
  headingMark: { alignItems: "center", justifyContent: "center", width: metrics.agentAvatarSize },
  headingLabel: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  headingCount: {
    color: colors.chromeDim,
    fontFamily: fonts.mono,
    fontSize: typeScale.meta,
    marginLeft: "auto",
  },
  toggleRow: { alignItems: "flex-end", paddingHorizontal: 16, paddingTop: 4 },
  toggle: { flexDirection: "row" },
  // 36 dp: the 32 dp pills plus the 2 dp inset the file viewer's track has.
  toggleTrack: { backgroundColor: colors.chromeRaised, borderRadius: radii.pill, bottom: 6, left: 0, position: "absolute", right: 0, top: 6 },
  segmentTarget: { paddingHorizontal: 2, paddingVertical: 8 },
  segment: { borderRadius: radii.pill - 2, minHeight: 32, justifyContent: "center", paddingHorizontal: 14 },
  // `accentWash` + `accent` is the app's "this segment is selected" treatment (files/ui/parts.tsx).
  segmentSelected: { backgroundColor: colors.accentWash },
  segmentLabel: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  segmentLabelSelected: { color: colors.accent },
});

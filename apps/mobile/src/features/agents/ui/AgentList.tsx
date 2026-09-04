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
import { MicIcon } from "../../../ui/components/MediaIcons";
import { useAnimationsAllowed } from "../../../ui/useAnimationsAllowed";
import { NotificationsOffBanner } from "../../notifications/ui/NotificationsOffBanner";
import { AGENT_LIST_MODES, buildAgentListItems, type AgentListItem, type AgentListMode } from "../agentListModel";
import { waitingColor } from "../agentViews";
import { useRecentIdleClock } from "../useRecentIdleClock";
import { PinIcon } from "./AgentIcon";
import { AgentMark, StateBadge } from "./AgentMark";

interface AgentListProps {
  /** Owned by the screen so the navigation call stays in one place. */
  onOpen(agent: Agent): void;
  onLongPress(agent: Agent): void;
  onTalk(agent: Agent): void;
  refreshing: boolean;
  onRefresh(): void;
  empty: ReactElement | null;
}

/** The Agents tab's list (design.md §9.3.1): the desktop's two orders, headed, and the priority order split by pin. */
export function AgentList({ onOpen, onLongPress, onTalk, refreshing, onRefresh, empty }: AgentListProps) {
  const agents = useSession((s) => s.agents);
  const sessions = useSession((s) => s.sessions);
  const windows = useSession((s) => s.windows);
  const adapters = useSession((s) => s.adapters);
  const mode = useStore(prefsStore, (s) => s.agentListMode);
  // One answer for every spinner on the tab: they turn only while someone can see them.
  const animate = useAnimationsAllowed();
  const agentList = useMemo(() => Object.values(agents), [agents]);
  // Workspace mode never moves a row on the Recent clock; the other two bucket or order by it.
  const revision = useRecentIdleClock(agentList, mode !== "workspace");
  const items = useMemo(
    () => buildAgentListItems({ agents, sessions, windows, adapters }, mode, Date.now()),
    // `revision` ticks when a Recent row ages out with no store update.
    [agents, sessions, windows, adapters, mode, revision],
  );

  return (
    <FlatList
      contentContainerStyle={items.length === 0 ? styles.fill : styles.content}
      data={items}
      extraData={animate}
      keyExtractor={(item) => item.key}
      ListEmptyComponent={empty}
      ListHeaderComponent={
        <>
          <NotificationsOffBanner />
          {items.length > 0 ? <ModeToggle mode={mode} onChange={(next) => prefsStore.getState().setAgentListMode(next)} /> : null}
        </>
      }
      refreshControl={<RefreshControl colors={[colors.accent]} progressBackgroundColor={colors.chromeRaised} onRefresh={onRefresh} refreshing={refreshing} />}
      renderItem={({ item, index }) => <Item animate={animate} index={index} item={item} items={items} onLongPress={onLongPress} onOpen={onOpen} onTalk={onTalk} />}
      style={styles.list}
    />
  );
}

function Item({ item, items, index, onOpen, onLongPress, onTalk, animate }: { item: AgentListItem; items: AgentListItem[]; index: number; onOpen(agent: Agent): void; onLongPress(agent: Agent): void; onTalk(agent: Agent): void; animate: boolean }) {
  const afterDivider = items[index - 1]?.kind === "divider";
  switch (item.kind) {
    case "divider":
      return <ListDivider afterRow={items[index - 1]?.kind === "agent"} first={index === 0} label={item.label} />;
    case "section":
      return (
        <GroupHeading afterDivider={afterDivider} count={item.count} label={item.label}>
          <StateBadge animate={animate} ink={colors.chromeDim} ring={colors.chromeBg} size={12} state={item.state} />
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
          accessibilityLabel={[
            item.title,
            item.agent.present ? STATE_WORDS[item.state] : "gone",
            item.waiting ? "waiting" : undefined,
            item.windowPinned ? "pinned window" : undefined,
            item.workspacePinned ? "pinned workspace" : undefined,
            // The desktop's label always names the workspace, even where the heading draws it.
            item.workspaceName,
          ].filter(Boolean).join(", ")}
          dimmed={!item.agent.present}
          // Blocked, or done and unseen: the mobile shape of the desktop's
          // "1" badge, in the colour the Workspaces tab paints the same agent.
          edgeColor={item.waiting ? waitingColor(item.waiting) : undefined}
          // A one-line row (Workspace mode) takes the Workspaces tab's 64 dp rather than sit a title alone in 76.
          height={item.subtitle === undefined ? metrics.windowRowHeight : metrics.agentRowHeight}
          leading={<AgentMark adapterId={item.agent.adapterId} animate={animate} state={item.state} />}
          onPress={() => onOpen(item.agent)}
          onLongPress={item.agent.route.sessionId ? () => onLongPress(item.agent) : undefined}
          subtitle={item.subtitle}
          title={item.title}
          // The window's pin only (the desktop's row pin). A pinned workspace
          // draws nothing on its rows: a second pin by the workspace's name read
          // as a second pinned thing. The spoken label still names it.
          titleAccessory={item.windowPinned ? <PinIcon color={colors.chromeDim} size={14} /> : null}
          // A gone agent has no live state to dock; the word is the only honest mark.
          trailing={item.agent.present
            ? item.agent.route.paneId ? <TalkButton onPress={() => onTalk(item.agent)} /> : undefined
            : <StatusPill state="gone" />}
        />
      );
  }
}

function TalkButton({ onPress }: { onPress(): void }) {
  return (
    <Pressable
      accessibilityLabel="Talk to agent"
      accessibilityRole="button"
      hitSlop={8}
      onPress={(event) => {
        event.stopPropagation();
        onPress();
      }}
      style={({ pressed }) => [styles.talk, pressed && styles.talkPressed]}
    >
      <MicIcon color={colors.accent} size={20} />
    </Pressable>
  );
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
 * collapsed-looking group has six agents in it; it shares the label's
 * baseline (the desktop heading's `align-items: baseline`), the mark is
 * centred on the line.
 */
function GroupHeading({ label, count, children, afterDivider }: { label: string; count: number; children?: React.ReactNode; afterDivider: boolean }) {
  return (
    <View
      // The desktop's <h3>: reachable by heading navigation, the count spoken as a count.
      accessibilityLabel={`${label}, ${count === 1 ? "1 agent" : `${count} agents`}`}
      accessibilityRole="header"
      style={[styles.heading, afterDivider && styles.headingAfterDivider]}
    >
      <View style={styles.headingMark}>{children}</View>
      <View style={styles.headingText}>
        <Text numberOfLines={1} style={styles.headingLabel}>{label}</Text>
        <Text style={styles.headingCount}>{count}</Text>
      </View>
    </View>
  );
}

/**
 * Priority | Workspace | Pinned — the desktop's sort toggle, as a segmented
 * control.
 *
 * Each segment's pressable is the full 48 dp touch target and the 32 dp pill
 * sits inside it; the track is painted behind the row rather than wrapping
 * it, because a hit slop never extends past the parent's bounds and a track
 * that wrapped the pills would have capped the target at their height.
 *
 * The track spans the list's width and the segments share it equally
 * (`flex: 1`): three labels at a fixed 100 dp each would not fit a 360 dp
 * phone inside the 16 dp margins, and equal thirds keep every segment the
 * same width whatever its label. Selection changes only colour: nothing about
 * the geometry depends on which segment is selected, so a tap cannot move the
 * control under the finger. The pill's corner radius is the track's minus the
 * 2 dp inset, so the two arcs are concentric and the pill never pokes through
 * the track's corner.
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
              <View style={styles.segment}>
                <View pointerEvents="none" style={[StyleSheet.absoluteFill, styles.segmentFill, { opacity: selected ? 1 : 0 }]} />
                <Text maxFontSizeMultiplier={1.3} numberOfLines={1} style={[styles.segmentLabel, selected && styles.segmentLabelSelected]}>{entry.label}</Text>
              </View>
            </Pressable>
          );
        })}
      </View>
    </View>
  );
}

/** The pill sits this far inside the track on every side. */
const TOGGLE_INSET = 2;

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
  headingText: { alignItems: "baseline", flex: 1, flexDirection: "row", gap: 12 },
  headingLabel: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  headingCount: {
    color: colors.chromeDim,
    fontFamily: fonts.mono,
    fontSize: typeScale.meta,
    marginLeft: "auto",
  },
  toggleRow: { paddingHorizontal: 16, paddingTop: 4 },
  toggle: { flexDirection: "row" },
  // 36 dp: the 32 dp pills plus the 2 dp inset the file viewer's track has.
  toggleTrack: { backgroundColor: colors.chromeRaised, borderRadius: radii.pill + TOGGLE_INSET, bottom: 6, left: 0, position: "absolute", right: 0, top: 6 },
  segmentTarget: { flex: 1, paddingHorizontal: TOGGLE_INSET, paddingVertical: 8 },
  // 8 dp of pill padding leaves ~89 dp for the label in a third of a 360 dp phone's track; `Workspace` at 13 sp 600 is ~64,
  // and the label caps its font scaling at 1.3× and never wraps, so the pill cannot grow past the track.
  segment: { alignItems: "center", borderRadius: radii.pill, minHeight: 32, justifyContent: "center", paddingHorizontal: 8 },
  // The selected wash is a fill layer mounted from the first frame and shown by opacity.
  // Adding a background to an already-mounted rounded view made Android redraw it with
  // square corners (QA row 43); a view that mounts with both keeps its arcs, and
  // opacity never rebuilds the background drawable.
  // `accentWash` + `accent` is the app's "this segment is selected" treatment (files/ui/parts.tsx).
  segmentFill: { backgroundColor: colors.accentWash, borderRadius: radii.pill },
  segmentLabel: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  segmentLabelSelected: { color: colors.accent },
  talk: { alignItems: "center", height: 40, justifyContent: "center", width: 40 },
  talkPressed: { opacity: 0.7 },
});

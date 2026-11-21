import { ActivityIndicator, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";

import type { HistoryEntry } from "../../../protocol/gen/history_pb";
import { EmptyState } from "../../../ui/components/EmptyState";
import { colors, fixedChromeText, typeScale } from "../../../ui/tokens";
import { AgentIcon } from "../../agents/ui/AgentIcon";
import { historyMeta, isSnoozed, type HistoryFilter } from "../historyModel";

export interface HistoryViewProps {
  rows: HistoryEntry[];
  /** Before the first LIST answers. */
  loading: boolean;
  error: string;
  /** Every recorded session, before the filter and the search. */
  total: number;
  filter: HistoryFilter;
  onFilter: (filter: HistoryFilter) => void;
  query: string;
  onQuery: (query: string) => void;
  /** The session being resumed or woken. */
  busyKey: string | undefined;
  now: number;
  onOpen: (entry: HistoryEntry) => void;
  onWake: (entry: HistoryEntry) => void;
}

export function entryKey(entry: Pick<HistoryEntry, "adapterId" | "nativeSessionId">): string {
  return `${entry.adapterId}\u0000${entry.nativeSessionId}`;
}

/** History (chat-mock.html, "Phone"): All or Snoozed, a search field, one row per session. */
export function HistoryView({ rows, loading, error, total, filter, onFilter, query, onQuery, busyKey, now, onOpen, onWake }: HistoryViewProps) {
  return (
    <View style={styles.root}>
      <View style={styles.top}>
        <TextInput
          accessibilityLabel="Search sessions"
          autoCapitalize="none"
          autoCorrect={false}
          clearButtonMode="while-editing"
          onChangeText={onQuery}
          placeholder="Search sessions"
          placeholderTextColor={colors.chromeFaint}
          returnKeyType="search"
          style={styles.search}
          value={query}
        />
        <View accessibilityRole="tablist" style={styles.seg}>
          {(["all", "snoozed"] as const).map((value) => (
            <Pressable
              accessibilityRole="tab"
              accessibilityState={{ selected: filter === value }}
              key={value}
              onPress={() => onFilter(value)}
              style={[styles.segItem, filter === value && styles.segItemOn]}
            >
              <Text {...fixedChromeText} style={[styles.segLabel, filter === value && styles.segLabelOn]}>{value === "all" ? "All" : "Snoozed"}</Text>
            </Pressable>
          ))}
        </View>
      </View>
      <ScrollView contentContainerStyle={styles.list} keyboardShouldPersistTaps="handled">
        {loading && total === 0 ? <ActivityIndicator color={colors.accent} style={styles.spinner} /> : null}
        {!loading && error ? <EmptyState heading="History isn't available" lines={[error]} /> : null}
        {!loading && !error && rows.length === 0 ? (
          <EmptyState
            heading={query ? "No matching sessions" : filter === "snoozed" ? "Nothing is snoozed" : "No sessions yet"}
            lines={query ? ["Search matches titles, workspaces and providers."] : filter === "snoozed" ? ["Snooze an agent from its long-press menu."] : ["Agent sessions the host sees show up here."]}
          />
        ) : null}
        {rows.map((entry) => (
          <HistoryRow busy={busyKey === entryKey(entry)} entry={entry} key={entryKey(entry)} now={now} onOpen={onOpen} onWake={onWake} />
        ))}
      </ScrollView>
    </View>
  );
}

function HistoryRow({ entry, now, busy, onOpen, onWake }: { entry: HistoryEntry; now: number; busy: boolean; onOpen: (entry: HistoryEntry) => void; onWake: (entry: HistoryEntry) => void }) {
  const meta = historyMeta(entry, now);
  const title = entry.title || "Untitled session";
  const snoozed = entry.snoozedUntilUnixMillis > 0n && !entry.woke;
  return (
    <Pressable
      accessibilityHint={entry.running ? "Opens it" : "Resumes it"}
      accessibilityLabel={`${title}, ${entry.workspaceName ? `${entry.workspaceName}, ` : ""}${snoozed ? `snoozed until ${meta}` : meta}`}
      accessibilityActions={snoozed && !busy ? [{ label: "Wake now", name: "wake" }] : undefined}
      accessibilityRole="button"
      disabled={busy}
      onAccessibilityAction={(event) => {
        if (event.nativeEvent.actionName === "wake") onWake(entry);
      }}
      onPress={() => onOpen(entry)}
      style={({ pressed }) => [styles.row, entry.woke && styles.rowWoke, pressed && styles.pressed]}
    >
      <View style={styles.chip}>
        <AgentIcon adapterId={entry.adapterId} color={colors.chromeDim} size={14} />
      </View>
      {isSnoozed(entry) ? <View style={styles.sleep} /> : null}
      <Text numberOfLines={1} style={styles.name}>{title}</Text>
      {busy ? <ActivityIndicator color={colors.accent} size="small" /> : (
        <Text {...fixedChromeText} numberOfLines={1} style={[styles.meta, entry.running && styles.metaRunning, entry.woke && styles.metaWoke]}>{meta}</Text>
      )}
      {snoozed && !busy ? (
        <Pressable accessibilityLabel={`Wake ${title} now`} accessibilityRole="button" hitSlop={8} onPress={() => onWake(entry)} style={({ pressed }) => [styles.wake, pressed && styles.pressed]}>
          <Text {...fixedChromeText} style={styles.wakeLabel}>Wake now</Text>
        </Pressable>
      ) : null}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  top: { alignItems: "center", flexDirection: "row", gap: 8, paddingBottom: 6, paddingHorizontal: 12, paddingTop: 8 },
  search: { borderColor: colors.chromeBorder, borderRadius: 16, borderWidth: 1, color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.body, minHeight: 40, paddingHorizontal: 12 },
  seg: { borderColor: colors.chromeBorder, borderRadius: 6, borderWidth: 1, flexDirection: "row", overflow: "hidden" },
  segItem: { alignItems: "center", justifyContent: "center", minHeight: 44, paddingHorizontal: 10 },
  segItemOn: { backgroundColor: colors.accentWash },
  segLabel: { color: colors.chromeDim, fontSize: typeScale.meta },
  segLabelOn: { color: colors.accent, fontWeight: "600" },
  list: { gap: 2, paddingBottom: 16, paddingHorizontal: 8 },
  spinner: { paddingTop: 24 },
  row: { alignItems: "center", borderRadius: 7, flexDirection: "row", gap: 8, minHeight: 48, paddingHorizontal: 8 },
  rowWoke: { backgroundColor: colors.accentWash, borderColor: colors.accent, borderWidth: 1 },
  pressed: { backgroundColor: colors.chromeHover },
  chip: { alignItems: "center", backgroundColor: colors.chromeRaised, borderColor: colors.chromeBorder, borderRadius: 5, borderWidth: 1, height: 22, justifyContent: "center", width: 22 },
  sleep: { borderColor: colors.chromeFaint, borderRadius: 4, borderStyle: "dashed", borderWidth: 1.5, height: 8, width: 8 },
  name: { color: colors.chromeInk, flex: 1, fontSize: typeScale.body, minWidth: 0 },
  meta: { color: colors.chromeDim, flexShrink: 0, fontSize: 11.5 },
  metaRunning: { color: colors.ok },
  metaWoke: { color: colors.accent, fontWeight: "600" },
  wake: { borderColor: colors.chromeBorder, borderRadius: 5, borderWidth: 1, justifyContent: "center", minHeight: 36, paddingHorizontal: 10 },
  wakeLabel: { color: colors.chromeDim, fontSize: 11.5 },
});

import { memo } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors, typeScale } from "../../../ui/tokens";
import { liveSteps, stepsLine } from "../steps";
import type { StepsRow as StepsRowModel } from "../timelineRows";
import { ChevronDownIcon, ChevronRightIcon } from "./ChatIcons";
import { useHeldOpen } from "./timelineOpen";
import { StepRow } from "./ToolRow";

/**
 * A run of tool calls and thinking as one line (mobile-chat-mock.html, S2):
 * how many steps, what kinds, and whether any failed. Tapping it shows every
 * step as a plain row under it. While its turn still works the line reads
 * "N steps so far" and, until it is opened, shows only the subagents that
 * run and the newest other step.
 */
export const StepsRow = memo(function StepsRow({ row }: { row: StepsRowModel }) {
  const [open, toggle] = useHeldOpen("groups", row.key);
  const line = stepsLine(row.items, row.live);
  const shown = open ? row.items : row.live ? liveSteps(row.items) : [];
  const Chevron = open ? ChevronDownIcon : ChevronRightIcon;
  return (
    <View>
      <Pressable
        accessibilityHint={open ? "Hides the steps" : "Shows every step"}
        accessibilityLabel={line.label}
        accessibilityRole="button"
        accessibilityState={{ expanded: open }}
        hitSlop={LINE_SLOP}
        onPress={toggle}
        style={({ pressed }) => [styles.line, pressed && styles.pressed]}
      >
        <Chevron color={colors.chromeDim} size={15} />
        <Text numberOfLines={1} style={styles.title}>{line.title}</Text>
        {/* The counts give way first: the title, a failure and the line counts stay whole. */}
        {line.counts ? <Text numberOfLines={1} style={styles.counts}>· {line.counts}</Text> : null}
        {line.failed ? <Text numberOfLines={1} style={styles.failed}>· {line.failed}</Text> : null}
        {line.added > 0 || line.removed > 0 ? (
          <Text numberOfLines={1} style={styles.lines}>
            <Text style={styles.added}>+{line.added}</Text> <Text style={styles.removed}>−{line.removed}</Text>
          </Text>
        ) : null}
      </Pressable>
      {shown.length > 0 ? (
        <View style={styles.rows}>
          {shown.map((item) => <StepRow item={item} key={item.id} />)}
        </View>
      ) : null}
    </View>
  );
});

const LINE_HEIGHT = 30;
// The line is slimmer than a touch target; the slop makes it 44 dp tall.
const LINE_SLOP = { bottom: (44 - LINE_HEIGHT) / 2, top: (44 - LINE_HEIGHT) / 2 } as const;

const styles = StyleSheet.create({
  line: { alignItems: "center", flexDirection: "row", gap: 6, minHeight: LINE_HEIGHT },
  pressed: { opacity: 0.6 },
  title: { color: colors.chromeDim, flexShrink: 0, fontSize: typeScale.rowSecondary },
  counts: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary, minWidth: 0 },
  failed: { color: colors.dangerInk, flexShrink: 0, fontSize: typeScale.rowSecondary },
  lines: { flexShrink: 0, fontSize: typeScale.meta },
  added: { color: colors.okInk },
  removed: { color: colors.dangerInk },
  // The steps hang from a rule under the chevron.
  rows: { borderLeftColor: colors.chromeHairline, borderLeftWidth: 2, marginLeft: 6, paddingLeft: 11 },
});

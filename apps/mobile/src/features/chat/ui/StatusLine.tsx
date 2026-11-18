import { StyleSheet, Text, View } from "react-native";

import { colors, fixedChromeText, stateColors, typeScale } from "../../../ui/tokens";
import { useAnimationsAllowed } from "../../../ui/useAnimationsAllowed";
import { Spinner } from "../../agents/ui/Spinner";
import type { ChatStatus } from "../chatStatus";
import { elapsed } from "../steps";
import { useRan } from "./useRan";

/**
 * The one line above the reply field that says the turn works
 * (mobile-chat-mock.html, S3): what the agent is doing, for how long, and how
 * many steps so far. It is outside the conversation, so scrolling and the
 * keyboard cannot hide it.
 */
export function StatusLine({ status }: { status: ChatStatus }) {
  const animate = useAnimationsAllowed();
  const ran = useRan(status.since);
  return (
    <View style={styles.line}>
      <Spinner animate={animate} ink={stateColors.working} ring={colors.chromeBg} size={14} />
      {/* Only the label is announced when it changes; the seconds would be read out every tick. */}
      <Text {...fixedChromeText} accessibilityLiveRegion="polite" numberOfLines={1} style={styles.label}>{status.label}</Text>
      {ran !== undefined ? <Text {...fixedChromeText} numberOfLines={1} style={styles.time}>{elapsed(ran)}</Text> : null}
      {status.steps ? <Text {...fixedChromeText} numberOfLines={1} style={styles.steps}>{status.steps}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create({
  line: { alignItems: "center", flexDirection: "row", gap: 8, minHeight: 30, paddingHorizontal: 14, paddingVertical: 4 },
  // The label gives way first: the time and the count stay whole.
  label: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary, minWidth: 0 },
  time: { color: colors.chromeFaint, flexShrink: 0, fontSize: typeScale.rowSecondary, fontVariant: ["tabular-nums"] },
  steps: { color: colors.chromeFaint, flexShrink: 0, fontSize: typeScale.rowSecondary },
});

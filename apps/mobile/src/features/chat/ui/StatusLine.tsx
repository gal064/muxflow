import { useIsFocused } from "expo-router";
import { useEffect, useState } from "react";
import { StyleSheet, Text, View } from "react-native";

import { colors, fixedChromeText, stateColors, typeScale } from "../../../ui/tokens";
import { useAnimationsAllowed } from "../../../ui/useAnimationsAllowed";
import { Spinner } from "../../agents/ui/Spinner";
import { elapsed, type ChatStatus } from "../chatStatus";

/**
 * The one line above the reply field that says the turn works
 * (mobile-chat-mock.html, S3): what the agent is doing, for how long, and how
 * many steps so far. It is outside the conversation, so scrolling and the
 * keyboard cannot hide it.
 */
export function StatusLine({ status }: { status: ChatStatus }) {
  const animate = useAnimationsAllowed();
  const time = useElapsed(status.since);
  return (
    <View style={styles.line}>
      <Spinner animate={animate} ink={stateColors.working} ring={colors.chromeBg} size={14} />
      {/* Only the label is announced when it changes; the seconds would be read out every tick. */}
      <Text {...fixedChromeText} accessibilityLiveRegion="polite" numberOfLines={1} style={styles.label}>{status.label}</Text>
      {time !== undefined ? <Text {...fixedChromeText} numberOfLines={1} style={styles.time}>{time}</Text> : null}
      {status.steps ? <Text {...fixedChromeText} numberOfLines={1} style={styles.steps}>{status.steps}</Text> : null}
    </View>
  );
}

/**
 * The time since `since`, as text that changes once a second while this
 * screen is the focused one. Undefined when there is nothing to time.
 */
function useElapsed(since: number | undefined): string | undefined {
  const focused = useIsFocused();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (since === undefined || !focused) return undefined;
    let timer: ReturnType<typeof setTimeout>;
    const tick = () => {
      const at = Date.now();
      setNow(at);
      // The next change of the seconds, not a second from whenever this ran.
      timer = setTimeout(tick, 1000 - (Math.max(0, at - since) % 1000));
    };
    tick();
    return () => clearTimeout(timer);
  }, [focused, since]);
  return since === undefined ? undefined : elapsed(now - since);
}

const styles = StyleSheet.create({
  line: { alignItems: "center", flexDirection: "row", gap: 8, minHeight: 30, paddingHorizontal: 14, paddingVertical: 4 },
  // The label gives way first: the time and the count stay whole.
  label: { color: colors.chromeDim, flexShrink: 1, fontSize: typeScale.rowSecondary, minWidth: 0 },
  time: { color: colors.chromeFaint, flexShrink: 0, fontSize: typeScale.rowSecondary, fontVariant: ["tabular-nums"] },
  steps: { color: colors.chromeFaint, flexShrink: 0, fontSize: typeScale.rowSecondary },
});

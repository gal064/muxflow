import { useEffect, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { colors, metrics, typeScale } from "../../ui/tokens";
import { backoffSeconds, connectingStripText, reconnectingStripText } from "./connectionLabels";
import { describeConnectionFailure } from "./errorMatrix";
import { useConnectionSheet } from "./ConnectionSheet";
import { useDiagnostics, useSession } from "./hooks";

/**
 * The connection strip — design.md §9 "Global chrome": 28 dp under the app bar
 * on every screen while the connection is not `connected`.
 */
export function ConnectionStrip() {
  const connection = useSession((state) => state.connection);
  const lastClose = useDiagnostics((state) => state.lastClose);
  const sheet = useConnectionSheet();
  const secondsLeft = useCountdown(connection.state, connection.attempt);

  if (connection.state === "connected" || connection.state === "idle") return null;

  if (connection.state === "failed" || connection.state === "incompatible") {
    const failure = describeConnectionFailure({
      state: connection.state,
      message: connection.message,
      close: lastClose ?? undefined,
      host: connection.host,
    });
    // The full-screen rows of §12 own their own surface; the strip carries the
    // rest, with the "Details" button §9 asks for.
    if (failure.presentation === "fullScreen") return null;
    return (
      <View style={[styles.strip, styles.danger]}>
        <Text style={[styles.text, styles.dangerText]} numberOfLines={1}>
          {failure.message}
        </Text>
        <Pressable onPress={sheet.open} hitSlop={8} accessibilityRole="button">
          <Text style={[styles.text, styles.details]}>Details</Text>
        </Pressable>
      </View>
    );
  }

  if (connection.state === "reconnecting") {
    return (
      // Only a close carries §12 copy; the state machine also reconnects for
      // reasons of its own ("sequence gap"), which §12 keeps out of the UI.
      <View style={[styles.strip, styles.warn]}>
        <Text style={[styles.text, styles.warnText]} numberOfLines={1}>
          {reconnectingStripText(secondsLeft, lastClose?.message)}
        </Text>
      </View>
    );
  }

  return (
    <View style={[styles.strip, styles.quiet]}>
      <Text style={[styles.text, styles.quietText]} numberOfLines={1}>
        {connectingStripText(connection.host?.label ?? "the host")}
      </Text>
    </View>
  );
}

/** Counts the §7.2 backoff down, so the strip's seconds match the retry. */
function useCountdown(state: string, attempt: number): number {
  const [secondsLeft, setSecondsLeft] = useState(0);
  useEffect(() => {
    if (state !== "reconnecting") {
      setSecondsLeft(0);
      return;
    }
    setSecondsLeft(backoffSeconds(attempt));
    const timer = setInterval(() => {
      setSecondsLeft((previous) => (previous > 0 ? previous - 1 : 0));
    }, 1000);
    return () => clearInterval(timer);
  }, [state, attempt]);
  return secondsLeft;
}

const styles = StyleSheet.create({
  strip: {
    alignItems: "center",
    flexDirection: "row",
    gap: 12,
    height: metrics.connectionStripHeight,
    paddingHorizontal: 16,
  },
  quiet: { backgroundColor: colors.chromeRaised },
  warn: { backgroundColor: "rgba(254, 188, 46, 0.2)" },
  danger: { backgroundColor: colors.dangerWash },
  text: { flexShrink: 1, fontSize: typeScale.meta },
  quietText: { color: colors.chromeDim },
  warnText: { color: colors.warn },
  dangerText: { color: colors.dangerInk },
  details: { color: colors.dangerInk, flexShrink: 0, fontWeight: "600" },
});

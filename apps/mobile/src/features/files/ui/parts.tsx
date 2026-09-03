// Small pieces the Files screens and the file viewer share.

import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Animated, Easing, Pressable, StyleSheet, Text, View } from "react-native";

import { colors, fixedChromeText, fonts, metrics, radii, typeScale } from "../../../ui/tokens";

/** The app-bar title block: title, and under it a single middle-ellipsised path. */
export function TitleBlock({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <View style={styles.titleBlock}>
      <Text {...fixedChromeText} style={styles.title} numberOfLines={1} ellipsizeMode="tail">
        {title}
      </Text>
      {subtitle ? (
        <Text {...fixedChromeText} style={styles.subtitle} numberOfLines={1} ellipsizeMode="middle">
          {subtitle}
        </Text>
      ) : null}
    </View>
  );
}

/** §9.6 step 1: a centred spinner while the root resolves and the listing loads. */
export function CentredSpinner() {
  return (
    <View style={styles.centred}>
      <ActivityIndicator color={colors.accent} />
    </View>
  );
}

/**
 * A centred line of copy: `Empty folder`, and the §9.7 placeholders.
 *
 * `quiet` is for a state that is merely uneventful — an empty directory. A
 * placeholder is the only thing on its screen and is the answer to "why can I
 * not see my file", so it is set in the reading ink, not the de-emphasised one.
 */
export function CentredMessage({ message, tone = "primary" }: { message: string; tone?: "primary" | "quiet" }) {
  return (
    <View style={styles.centred}>
      <Text style={tone === "quiet" ? styles.messageQuiet : styles.message}>{message}</Text>
    </View>
  );
}

/** The error state §9.6 step 7 and §9.7 step 3 both describe: a message and `Retry`. */
export function ErrorState({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <View style={styles.centred}>
      <Text style={styles.error}>{message}</Text>
      <Pressable
        accessibilityRole="button"
        onPress={onRetry}
        style={({ pressed }) => [styles.retry, pressed && styles.retryPressed]}
      >
        <Text style={styles.retryLabel}>Retry</Text>
      </Pressable>
    </View>
  );
}

/**
 * §9.7 step 2: a thin indeterminate progress bar under the app bar while
 * streaming.
 *
 * The travel is measured from the track and interpolated to dp. A percentage
 * `outputRange` is not something the native driver can evaluate — it forwards
 * the string unconverted, the Android node's numeric value stays `NaN`, and the
 * bar silently never moves.
 */
export function StreamingBar() {
  const progress = useRef(new Animated.Value(0)).current;
  const [width, setWidth] = useState(0);
  useEffect(() => {
    if (width === 0) return;
    progress.setValue(0);
    const animation = Animated.loop(
      Animated.timing(progress, { toValue: 1, duration: 1100, easing: Easing.linear, useNativeDriver: true }),
    );
    animation.start();
    return () => animation.stop();
  }, [progress, width]);
  const barWidth = width * PROGRESS_BAR_FRACTION;
  return (
    <View style={styles.progressTrack} onLayout={(event) => setWidth(event.nativeEvent.layout.width)}>
      {width > 0 ? (
        <Animated.View
          style={[
            styles.progressBar,
            {
              width: barWidth,
              transform: [
                { translateX: progress.interpolate({ inputRange: [0, 1], outputRange: [-barWidth, width] }) },
              ],
            },
          ]}
        />
      ) : null}
    </View>
  );
}

const PROGRESS_BAR_FRACTION = 0.4;

const styles = StyleSheet.create({
  titleBlock: {
    justifyContent: "center",
    minWidth: 0,
  },
  title: {
    color: colors.chromeInkStrong,
    fontSize: typeScale.appBarTitle,
    fontWeight: "600",
  },
  subtitle: {
    color: colors.chromeDim,
    fontFamily: fonts.mono,
    fontSize: typeScale.meta,
    marginTop: 1,
  },
  centred: {
    alignItems: "center",
    flex: 1,
    gap: 16,
    justifyContent: "center",
    padding: 24,
  },
  message: {
    color: colors.chromeInk,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  messageQuiet: {
    color: colors.chromeDim,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  error: {
    color: colors.dangerInk,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  // The only action on its screen: a filled accent button, and 48 dp tall so it
  // meets the platform's minimum tap target. `accentWash` is the "this segment
  // is selected" treatment and must not read as a button too.
  retry: {
    alignItems: "center",
    backgroundColor: colors.accent,
    borderRadius: radii.pill,
    justifyContent: "center",
    minHeight: 48,
    paddingHorizontal: 24,
  },
  retryPressed: {
    opacity: 0.8,
  },
  retryLabel: {
    color: colors.accentInk,
    fontSize: typeScale.body,
    fontWeight: "600",
  },
  progressTrack: {
    backgroundColor: colors.chromeHairline,
    height: 2,
    overflow: "hidden",
    width: "100%",
  },
  progressBar: {
    backgroundColor: colors.accent,
    height: metrics.hairlineWidth * 2,
  },
});

// Small pieces the Files screens and the file viewer share.

import { useEffect, useRef } from "react";
import { ActivityIndicator, Animated, Easing, Pressable, StyleSheet, Text, View } from "react-native";

import { colors, fonts, metrics, radii, typeScale } from "../../../ui/tokens";

/** The app-bar title block: title, and under it a single middle-ellipsised path. */
export function TitleBlock({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <View style={styles.titleBlock}>
      <Text style={styles.title} numberOfLines={1} ellipsizeMode="tail">
        {title}
      </Text>
      {subtitle ? (
        <Text style={styles.subtitle} numberOfLines={1} ellipsizeMode="middle">
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

/** A centred line of dim copy: `Empty folder`, and the §9.7 placeholders. */
export function CentredMessage({ message }: { message: string }) {
  return (
    <View style={styles.centred}>
      <Text style={styles.message}>{message}</Text>
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

/** §9.7 step 2: a thin indeterminate progress bar under the app bar while streaming. */
export function StreamingBar() {
  const progress = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    const animation = Animated.loop(
      Animated.timing(progress, { toValue: 1, duration: 1100, easing: Easing.linear, useNativeDriver: true }),
    );
    animation.start();
    return () => animation.stop();
  }, [progress]);
  return (
    <View style={styles.progressTrack}>
      <Animated.View
        style={[
          styles.progressBar,
          {
            transform: [
              { translateX: progress.interpolate({ inputRange: [0, 1], outputRange: ["-40%", "260%"] }) },
            ],
          },
        ]}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  titleBlock: {
    justifyContent: "center",
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
    color: colors.chromeDim,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  error: {
    color: colors.dangerInk,
    fontSize: typeScale.body,
    textAlign: "center",
  },
  retry: {
    backgroundColor: colors.accentWash,
    borderRadius: radii.pill,
    paddingHorizontal: 20,
    paddingVertical: 10,
  },
  retryPressed: {
    backgroundColor: colors.chromeSelected,
  },
  retryLabel: {
    color: colors.accent,
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
    width: "40%",
  },
});

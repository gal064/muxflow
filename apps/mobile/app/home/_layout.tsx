import { Tabs } from "expo-router";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { blockedAgentCount, needsAttention } from "../../src/store/selectors";
import { ConnectionStrip } from "../../src/ui/components/ConnectionStrip";
import { useSession } from "../../src/ui/hooks";
import { colors, metrics, typeScale } from "../../src/ui/tokens";

/** Home — design.md §9.3. Two tabs; Agents is the initial tab. */
export default function HomeLayout() {
  const label = useSession((s) => s.connection.host?.label ?? "Muxflow");
  const badge = useSession((s) => Object.values(s.agents).filter((a) => a.present && needsAttention(a) && a.lifecycle === "blocked").length);
  // Kept for parity with the desktop's badge rule; the tab badge is the stricter count above.
  void blockedAgentCount;
  return (
    <Tabs
      screenOptions={{
        headerStyle: { backgroundColor: colors.chromeBg, height: metrics.appBarHeight + STATUS_BAR_ALLOWANCE },
        headerTintColor: colors.chromeInkStrong,
        headerTitle: label,
        headerTitleStyle: {
          color: colors.chromeInkStrong,
          fontSize: typeScale.appBarTitle,
          fontWeight: "600",
        },
        headerShadowVisible: false,
        headerRight: () => <ConnectionDot />,
        sceneStyle: { backgroundColor: colors.chromeBg },
        tabBarStyle: {
          backgroundColor: colors.chromeRaised,
          borderTopColor: colors.chromeHairline,
          borderTopWidth: metrics.hairlineWidth,
          height: metrics.tabBarHeight,
        },
        tabBarActiveTintColor: colors.accent,
        tabBarInactiveTintColor: colors.chromeDim,
        tabBarLabelStyle: { fontSize: typeScale.meta },
      }}
    >
      <Tabs.Screen name="index" options={{ href: null }} />
      <Tabs.Screen
        name="agents"
        options={{
          title: "Agents",
          tabBarIcon: ({ color }) => <Text style={[styles.icon, { color }]}>✦</Text>,
          tabBarBadge: badge > 0 ? badge : undefined,
          tabBarBadgeStyle: { backgroundColor: colors.danger, color: "#ffffff", fontSize: 10, fontWeight: "700" },
          header: (props) => <TabHeader title={label} {...props} />,
        }}
      />
      <Tabs.Screen
        name="workspaces"
        options={{
          title: "Workspaces",
          tabBarIcon: ({ color }) => <Text style={[styles.icon, { color }]}>❐</Text>,
          header: (props) => <TabHeader title={label} {...props} />,
        }}
      />
    </Tabs>
  );
}

const STATUS_BAR_ALLOWANCE = 0;

/** App bar (56 dp) with the host label and the connection dot, then the global strip. */
function TabHeader({ title }: { title: string; [key: string]: unknown }) {
  return (
    <View style={styles.headerWrap}>
      <View style={styles.header}>
        <Text style={styles.headerTitle} numberOfLines={1}>{title}</Text>
        <ConnectionDot />
      </View>
      <ConnectionStrip />
    </View>
  );
}

/** 10 dp: `--ok` connected, `--warn` reconnecting/connecting, `--danger` failed. Opens the Connection sheet (§9.8, M6). */
function ConnectionDot() {
  const state = useSession((s) => s.connection.state);
  const color = state === "connected" ? colors.ok : state === "failed" || state === "incompatible" ? colors.danger : colors.warn;
  return (
    <Pressable accessibilityLabel={`Connection: ${state}`} hitSlop={12} style={styles.dotHit}>
      <View style={[styles.dot, { backgroundColor: color }]} />
    </Pressable>
  );
}

const styles = StyleSheet.create({
  icon: { fontSize: 18 },
  headerWrap: { backgroundColor: colors.chromeBg },
  header: {
    alignItems: "center",
    flexDirection: "row",
    height: metrics.appBarHeight,
    paddingHorizontal: 16,
  },
  headerTitle: {
    color: colors.chromeInkStrong,
    flex: 1,
    fontSize: typeScale.appBarTitle,
    fontWeight: "600",
  },
  dotHit: { padding: 8 },
  dot: { borderRadius: 5, height: 10, width: 10 },
});

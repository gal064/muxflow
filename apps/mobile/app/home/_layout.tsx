import { Tabs, useRouter } from "expo-router";
import { Pressable, StyleSheet, Text, View } from "react-native";

import { useSafeAreaInsets } from "react-native-safe-area-context";

import { waitingCount } from "../../src/store/selectors";
import { ConnectionDot } from "../../src/features/hosts/ConnectionDot";
import { ConnectionStrip } from "../../src/features/hosts/ConnectionStrip";
import { UpdatePill } from "../../src/features/update/ui/UpdatePill";
import { SettingsIcon } from "../../src/ui/components/MediaIcons";
import { useSession } from "../../src/ui/hooks";
import { colors, fixedChromeText, metrics, typeScale } from "../../src/ui/tokens";

/** Home — design.md §9.3. Two tabs; Agents is the initial tab. */
export default function HomeLayout() {
  const insets = useSafeAreaInsets();
  const label = useSession((s) => s.connection.host?.label ?? "Muxflow");
  // The desktop's bell count (`unreadCount`): every agent that is blocked, or done and not yet looked at.
  const badge = useSession(waitingCount);
  return (
    <Tabs
      screenOptions={{
        headerStyle: { backgroundColor: colors.chromeBg },
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
          // 56 dp of bar above the gesture-navigation inset (edge-to-edge window).
          height: metrics.tabBarHeight + insets.bottom,
          paddingBottom: insets.bottom,
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

/** App bar (56 dp below the status bar) with the host label, the update pill while a newer release is out, and the connection dot, then the global strip. */
function TabHeader({ title }: { title: string; [key: string]: unknown }) {
  const insets = useSafeAreaInsets();
  const router = useRouter();
  return (
    <View style={[styles.headerWrap, { paddingTop: insets.top }]}>
      <View style={styles.header}>
        <Text {...fixedChromeText} style={styles.headerTitle} numberOfLines={1}>{title}</Text>
        <UpdatePill />
        <Pressable accessibilityLabel="Settings" accessibilityRole="button" onPress={() => router.push("/settings")} style={styles.headerAction}>
          <SettingsIcon color={colors.chromeDim} />
        </Pressable>
        <ConnectionDot />
      </View>
      <ConnectionStrip />
    </View>
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
    minWidth: 0,
  },
  headerAction: { alignItems: "center", height: 48, justifyContent: "center", width: 48 },
});

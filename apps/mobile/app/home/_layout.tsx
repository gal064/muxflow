import { Tabs } from "expo-router";

import { colors, metrics, typeScale } from "../../src/ui/tokens";

/** Home — design.md §9.3. Two tabs; Agents is the initial tab. */
export default function HomeLayout() {
  return (
    <Tabs
      screenOptions={{
        headerStyle: { backgroundColor: colors.chromeBg },
        headerTintColor: colors.chromeInkStrong,
        headerTitleStyle: {
          color: colors.chromeInkStrong,
          fontSize: typeScale.appBarTitle,
          fontWeight: "600",
        },
        headerShadowVisible: false,
        sceneStyle: { backgroundColor: colors.chromeBg },
        tabBarStyle: {
          backgroundColor: colors.chromeRaised,
          borderTopColor: colors.chromeHairline,
          borderTopWidth: metrics.hairlineWidth,
          height: metrics.tabBarHeight,
        },
        tabBarActiveTintColor: colors.accent,
        tabBarInactiveTintColor: colors.chromeDim,
        // §9.3 gives each tab an icon (sparkle / layers). Until those exist,
        // render nothing rather than the default missing-glyph box.
        tabBarIcon: () => null,
      }}
    >
      <Tabs.Screen name="index" options={{ href: null }} />
      <Tabs.Screen name="agents" options={{ title: "Agents" }} />
      <Tabs.Screen name="workspaces" options={{ title: "Workspaces" }} />
    </Tabs>
  );
}

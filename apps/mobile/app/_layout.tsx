import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";
import { KeyboardProvider } from "react-native-keyboard-controller";

import { wireApp } from "../src/session/appWiring";
import { colors, typeScale } from "../src/ui/tokens";

wireApp();

/**
 * Root navigator. Global chrome per design.md §9: the status bar and every
 * app bar sit on `--chrome-bg`, titles are 17 sp semibold `--chrome-ink-strong`.
 * The Terminal screen is the one screen without a standard app bar (§9.5).
 */
export default function RootLayout() {
  return (
    <KeyboardProvider>
      <StatusBar style="light" />
      <Stack
        screenOptions={{
          headerStyle: { backgroundColor: colors.chromeBg },
          headerTintColor: colors.chromeInkStrong,
          headerTitleStyle: {
            color: colors.chromeInkStrong,
            fontSize: typeScale.appBarTitle,
            fontWeight: "600",
          },
          headerShadowVisible: false,
          contentStyle: { backgroundColor: colors.chromeBg },
        }}
      >
        <Stack.Screen name="index" options={{ title: "Muxflow" }} />
        <Stack.Screen name="hosts/[id]" options={{ title: "Add host" }} />
        <Stack.Screen name="home" options={{ headerShown: false }} />
        <Stack.Screen name="workspace/[sessionId]" options={{ title: "Workspace" }} />
        <Stack.Screen name="terminal/[paneId]" options={{ headerShown: false }} />
        <Stack.Screen name="files/[paneId]/index" options={{ title: "Files" }} />
        <Stack.Screen name="files/[paneId]/dir" options={{ title: "Files" }} />
        <Stack.Screen name="file/[paneId]" options={{ title: "File" }} />
        <Stack.Screen name="key" options={{ title: "Your SSH key" }} />
      </Stack>
    </KeyboardProvider>
  );
}

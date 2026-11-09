import Constants from "expo-constants";
import { Stack } from "expo-router";
import { Platform, Pressable, StyleSheet, Switch, Text, TextInput, View } from "react-native";
import { useStore } from "zustand";

import { TERMINAL_FONT_SIZES } from "../src/features/terminal/sizing";
import { openAppUpdates, updateStore } from "../src/features/update";
import { prefsStore } from "../src/store/prefsStore";
import { colors, fixedChromeText, radii, typeScale } from "../src/ui/tokens";

/** App preferences that belong to this phone. */
export default function SettingsScreen() {
  const agentCommand = useStore(prefsStore, (state) => state.agentCommand);
  const terminalFontSize = useStore(prefsStore, (state) => state.terminalFontSize);
  const voiceKeepAwake = useStore(prefsStore, (state) => state.voiceKeepAwake);
  const voicePlayScreenOff = useStore(prefsStore, (state) => state.voicePlayScreenOff);
  const update = useStore(updateStore, (state) => state.update);
  const downloading = useStore(updateStore, (state) => state.downloading);
  return (
    <View style={styles.root}>
      <Stack.Screen options={{ title: "Settings" }} />
      <View style={styles.section}>
        <Text {...fixedChromeText} style={styles.label}>Agent command</Text>
        <TextInput
          autoCapitalize="none"
          autoCorrect={false}
          maxLength={4 * 1024}
          onChangeText={(command) => prefsStore.getState().setAgentCommand(command)}
          placeholder="codex"
          placeholderTextColor={colors.chromeFaint}
          returnKeyType="done"
          style={styles.input}
          value={agentCommand}
        />
        <Text style={styles.help}>New agent starts this command in a new terminal window. Shell options and arguments are allowed.</Text>
      </View>
      <View style={styles.settingRow}>
        <View style={styles.settingCopy}>
          <Text {...fixedChromeText} style={styles.label}>Terminal font size</Text>
          <Text style={styles.help}>Larger text fits fewer columns.</Text>
        </View>
        <View style={styles.sizeOptions}>
          {TERMINAL_FONT_SIZES.map((size) => (
            <Pressable
              accessibilityLabel={`Terminal font size ${size}`}
              accessibilityRole="button"
              accessibilityState={{ selected: size === terminalFontSize }}
              key={size}
              onPress={() => prefsStore.getState().setTerminalFontSize(size)}
              style={[styles.sizeOption, size === terminalFontSize && styles.sizeOptionSelected]}
            >
              <Text {...fixedChromeText} style={styles.sizeLabel}>{size}</Text>
            </Pressable>
          ))}
        </View>
      </View>
      <View style={styles.settingRow}>
        <View style={styles.settingCopy}>
          <Text {...fixedChromeText} style={styles.label}>Keep screen awake in Voice</Text>
          <Text style={styles.help}>Prevents screen sleep only while the Voice screen is open.</Text>
        </View>
        <Switch
          accessibilityLabel="Keep screen awake in Voice"
          onValueChange={(value) => prefsStore.getState().setVoiceKeepAwake(value)}
          thumbColor={voiceKeepAwake ? colors.accent : colors.chromeDim}
          trackColor={{ false: colors.chromeBorder, true: colors.accentWash }}
          value={voiceKeepAwake}
        />
      </View>
      {Platform.OS === "android" ? (
        <View style={styles.settingRow}>
          <View style={styles.settingCopy}>
            <Text {...fixedChromeText} style={styles.label}>Play replies with screen off</Text>
            <Text style={styles.help}>Keeps playing replies when you turn the phone off on the Voice screen.</Text>
          </View>
          <Switch
            accessibilityLabel="Play replies with screen off"
            onValueChange={(value) => prefsStore.getState().setVoicePlayScreenOff(value)}
            thumbColor={voicePlayScreenOff ? colors.accent : colors.chromeDim}
            trackColor={{ false: colors.chromeBorder, true: colors.accentWash }}
            value={voicePlayScreenOff}
          />
        </View>
      ) : null}
      <View style={styles.settingRow}>
        <Text style={styles.help}>Version {Constants.expoConfig?.version ?? "unknown"}</Text>
        {Platform.OS === "ios" ? (
          <Text accessibilityRole="link" onPress={() => void openAppUpdates()} style={styles.updateLink}>Open TestFlight</Text>
        ) : update && (
          <Text accessibilityRole="link" onPress={() => void openAppUpdates()} style={styles.updateLink}>
            {downloading ? "Downloading…" : `${update.version} available`}
          </Text>
        )}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  section: { gap: 8, padding: 16 },
  settingRow: { alignItems: "center", borderTopColor: colors.chromeBorder, borderTopWidth: StyleSheet.hairlineWidth, flexDirection: "row", gap: 16, marginHorizontal: 16, paddingVertical: 16 },
  settingCopy: { flex: 1, gap: 4 },
  label: { color: colors.chromeInkStrong, fontSize: typeScale.rowTitle, fontWeight: "600" },
  input: {
    backgroundColor: colors.chromeRaised,
    borderColor: colors.chromeBorder,
    borderRadius: radii.card,
    borderWidth: StyleSheet.hairlineWidth,
    color: colors.chromeInkStrong,
    fontSize: typeScale.body,
    minHeight: 48,
    paddingHorizontal: 12,
  },
  help: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  sizeOptions: { flexDirection: "row", gap: 4 },
  sizeOption: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: radii.pill, borderWidth: StyleSheet.hairlineWidth, justifyContent: "center", minHeight: 44, minWidth: 40 },
  sizeOptionSelected: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  sizeLabel: { color: colors.chromeInkStrong, fontSize: typeScale.body },
  updateLink: { color: colors.danger, fontSize: typeScale.rowSecondary, fontWeight: "600" },
});

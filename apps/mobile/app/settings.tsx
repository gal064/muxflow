import Constants from "expo-constants";
import { Stack } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { Platform, Pressable, ScrollView, StyleSheet, Switch, Text, TextInput, View } from "react-native";
import { useStore } from "zustand";

import { TERMINAL_FONT_SIZES } from "../src/features/terminal/sizing";
import { openAppUpdates, updateStore } from "../src/features/update";
import { refreshAgents } from "../src/features/agents/refresh";
import { agentModeLabel } from "../src/features/chat/chatModel";
import { useSession } from "../src/ui/hooks";
import { findHost, hostsStore, isStartDirectory, type SavedHost } from "../src/store/hostsStore";
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
  const connected = useSession((state) => state.connection.state === "connected");
  const agentMode = useSession((state) => state.agentMode);
  const connectedHostId = useSession((state) => (state.connection.state === "connected" ? state.connection.host?.id : undefined));
  const connectedHost = useStore(hostsStore, (state) => findHost(state, connectedHostId));
  // The host tells only the desktop that set it; its next agent snapshot carries the mode.
  useEffect(() => {
    if (connected) void refreshAgents();
  }, [connected]);
  return (
    <ScrollView keyboardShouldPersistTaps="handled" style={styles.root}>
      <Stack.Screen options={{ title: "Settings" }} />
      <Text {...fixedChromeText} style={styles.sectionHeading}>New sessions</Text>
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
      {connectedHost ? <DefaultDirectory host={connectedHost} key={connectedHost.id} /> : null}
      {connected && agentMode !== "unknown" ? (
        <View accessible accessibilityLabel={`Agents start as ${agentModeLabel(agentMode)}. Change it on your desktop app.`} style={styles.settingRow}>
          <View style={styles.settingCopy}>
            <Text {...fixedChromeText} style={styles.label}>Agents start as</Text>
            <Text style={styles.help}>Set for this host. Change it on your desktop app.</Text>
          </View>
          <Text {...fixedChromeText} style={styles.readOnlyValue}>{agentModeLabel(agentMode)}</Text>
        </View>
      ) : null}
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
          <Text {...fixedChromeText} style={styles.label}>Keep screen awake in voice mode</Text>
          <Text style={styles.help}>Prevents screen sleep only while voice mode is on.</Text>
        </View>
        <Switch
          accessibilityLabel="Keep screen awake in voice mode"
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
            <Text style={styles.help}>Keeps playing replies when you turn the phone off in voice mode.</Text>
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
    </ScrollView>
  );
}

/** Where New agent and New terminal start the `random` workspace on the connected host. */
function DefaultDirectory({ host }: { host: SavedHost }) {
  const [draft, setDraft] = useState(host.defaultDirectory ?? "");
  const [refused, setRefused] = useState(false);
  const latest = useRef(draft);
  latest.current = draft;
  // Written on blur, on Done and when Settings closes with the field still focused.
  // A relative path is never saved: the host would refuse it at create time.
  const commit = useCallback(() => {
    const valid = isStartDirectory(latest.current);
    setRefused(!valid);
    if (!valid) return;
    const current = hostsStore.getState().hosts.find((saved) => saved.id === host.id);
    if (!current || (current.defaultDirectory ?? "") === latest.current.trim()) return;
    hostsStore.getState().updateHost(current.id, { host: current.host, port: current.port, user: current.user, label: current.label, defaultDirectory: latest.current });
  }, [host.id]);
  useEffect(() => commit, [commit]);
  return (
    <View style={[styles.section, styles.sectionFollows]}>
      <Text {...fixedChromeText} style={styles.label}>Default directory</Text>
      <TextInput
        accessibilityLabel={`Default directory on ${host.label}`}
        autoCapitalize="none"
        autoCorrect={false}
        maxLength={4 * 1024}
        onChangeText={(text) => {
          setDraft(text);
          setRefused(false);
        }}
        onEndEditing={commit}
        onSubmitEditing={commit}
        placeholder="~"
        placeholderTextColor={colors.chromeFaint}
        returnKeyType="done"
        style={styles.input}
        testID="settings-default-directory"
        value={draft}
      />
      {refused ? <Text accessibilityLiveRegion="polite" style={styles.error}>Not saved: use an absolute path, ~ or a path under ~/.</Text> : null}
      <Text style={styles.help}>New agent and New terminal open in the random workspace, starting here on {host.label}. An absolute path or one under ~.</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  section: { gap: 8, padding: 16 },
  sectionFollows: { paddingTop: 0 },
  sectionHeading: { color: colors.chromeDim, fontSize: typeScale.meta, fontWeight: "600", letterSpacing: 0.5, paddingHorizontal: 16, paddingTop: 16, textTransform: "uppercase" },
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
  error: { color: colors.dangerInk, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  help: { color: colors.chromeDim, fontSize: typeScale.rowSecondary, lineHeight: 18 },
  sizeOptions: { flexDirection: "row", gap: 4 },
  sizeOption: { alignItems: "center", borderColor: colors.chromeBorder, borderRadius: radii.pill, borderWidth: StyleSheet.hairlineWidth, justifyContent: "center", minHeight: 44, minWidth: 40 },
  sizeOptionSelected: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  sizeLabel: { color: colors.chromeInkStrong, fontSize: typeScale.body },
  readOnlyValue: { color: colors.chromeDim, fontSize: typeScale.body },
  updateLink: { color: colors.danger, fontSize: typeScale.rowSecondary, fontWeight: "600" },
});

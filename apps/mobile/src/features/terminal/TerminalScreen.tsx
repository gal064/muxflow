import { useFocusEffect, useRouter } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import Animated, { useAnimatedKeyboard, useAnimatedStyle } from "react-native-reanimated";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { agentForPane, agentPillState } from "../agents/agentViews";
import { stripAgentStatusGlyphs } from "../agents/agentLabels";
import { toRouteParam } from "../../navigation/routeParams";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { sessionStore } from "../../store/sessionStore";
import { ConnectionStrip } from "../hosts/ConnectionStrip";
import { FolderIcon, MicIcon } from "../../ui/components/MediaIcons";
import { StatusPill } from "../../ui/components/StatusPill";
import { useSession } from "../../ui/hooks";
import { colors, metrics, radii, typeScale } from "../../ui/tokens";
import { appForeground } from "./appForeground";
import type { FromPageMessage } from "./bridgeMessages";
import { KEY_CHIPS, SHIFT_CHIP, pressChip } from "./chips";
import { TerminalController, type TerminalSnapshot } from "./TerminalController";
import { terminalRegistry } from "./terminalRegistry";
import { TerminalWebView, type TerminalWebViewHandle } from "./TerminalWebView";

export interface TerminalScreenProps {
  paneId: string;
  sessionId: string;
}

/** Terminal — design.md §9.5. Header, xterm WebView, key chips, input bar. */
export function TerminalScreen({ paneId, sessionId }: TerminalScreenProps) {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  // Expo SDK 57 is edge-to-edge on Android, where adjustResize no longer
  // shrinks the window and RN's Keyboard events under-report the height.
  // Reanimated reads the IME inset itself; padding by it keeps the chips and
  // input bar visible and shrinks the WebView, which re-measures and sends
  // RESIZE_TERMINAL (§7.6 step 6).
  const keyboard = useAnimatedKeyboard({ isStatusBarTranslucentAndroid: true, isNavigationBarTranslucentAndroid: true });
  const keyboardPadding = useAnimatedStyle(() => ({ paddingBottom: Math.max(insets.bottom, keyboard.height.value) }), [insets.bottom]);
  const state = useSession((s) => s);
  const webview = useRef<TerminalWebViewHandle>(null);
  const controller = useRef<TerminalController | null>(null);
  const [snapshot, setSnapshot] = useState<TerminalSnapshot>({ phase: "preparing", grid: undefined, lastError: undefined });
  const [pageLoaded, setPageLoaded] = useState(false);
  const [text, setText] = useState("");
  // §9.5: the ⇧ chip arms Shift for the next chip only; Send also disarms it.
  const [shiftArmed, setShiftArmed] = useState(false);
  const pane = state.panes[paneId];
  const connected = state.connection.state === "connected";
  // §9.5: the pane left the topology (window closed elsewhere), or never was
  // in it (a retained "gone" agent). New terminal waits for its pane before
  // navigating here, so an absent pane on a loaded topology is conclusive.
  const gone = connected && !pane && state.topologyGeneration > 0n;
  const agent = agentForPane(state, paneId);
  const session = state.sessions[sessionId];
  const window = pane ? state.windows[pane.windowId] : undefined;
  const title = agent?.displayName
    || `${stripAgentStatusGlyphs(session?.name ?? sessionId)} · ${stripAgentStatusGlyphs(window?.name ?? "")}`.replace(/ · $/, "");

  // One controller per focus: hide on blur (Files, back), re-attach on focus (§7.6 steps 1, 4).
  useFocusEffect(useCallback(() => {
    if (!pageLoaded || gone) return;
    const instance = new TerminalController({
      paneId,
      sessionId,
      store: sessionStore,
      registry: terminalRegistry,
      getConnection,
      page: { send: (message) => webview.current?.send(message) },
      onChange: setSnapshot,
      log,
      foreground: appForeground,
    });
    controller.current = instance;
    instance.start();
    setSnapshot(instance.snapshot);
    return () => {
      controller.current = null;
      void instance.stop();
    };
  }, [gone, pageLoaded, paneId, sessionId]));

  const onPageMessage = useCallback((message: FromPageMessage) => {
    controller.current?.onPageMessage(message);
  }, []);

  const toastError = (error: unknown) => toast(error instanceof Error ? error.message : String(error));

  const send = useCallback((bytes: Uint8Array) => {
    const instance = controller.current;
    if (!instance) return;
    instance.sendInput(bytes).catch(toastError);
  }, []);

  // Clear the field before the submit round trip so a second Send cannot
  // resend the same text; the controller pastes the body, then sends the CR.
  const sendText = useCallback(() => {
    const instance = controller.current;
    if (!instance) return;
    const body = text;
    setText("");
    setShiftArmed(false);
    instance.submitText(body).catch(toastError);
  }, [text]);

  const onChip = useCallback((chip: (typeof KEY_CHIPS)[number]) => {
    const press = pressChip(chip, shiftArmed);
    setShiftArmed(press.shiftArmed);
    if (press.send) send(press.send);
  }, [send, shiftArmed]);

  useEffect(() => {
    if (snapshot.lastError) toast(snapshot.lastError);
  }, [snapshot.lastError]);

  const inputEnabled = connected && !gone && snapshot.phase !== "exited";

  return (
    <Animated.View style={[styles.root, { paddingTop: insets.top }, keyboardPadding]}>
      <ConnectionStrip />
      <View style={styles.header}>
        <Pressable accessibilityLabel="Back" accessibilityRole="button" onPress={() => router.back()} style={styles.iconButton}>
          <Text style={styles.backGlyph}>←</Text>
        </Pressable>
        <Text numberOfLines={1} style={styles.title}>{title}</Text>
        {agent ? <StatusPill state={agentPillState(agent)} /> : null}
        {gone || !agent ? null : (
          <Pressable
            accessibilityLabel="Talk to this agent"
            accessibilityRole="button"
            onPress={() => router.push({ pathname: "/voice/[paneId]", params: { paneId: toRouteParam(paneId), sessionId: toRouteParam(sessionId), agentId: toRouteParam(agent.id) } })}
            style={styles.iconButton}
          >
            <MicIcon color={colors.accent} size={22} />
          </Pressable>
        )}
        {gone ? null : (
          <Pressable
            accessibilityLabel="Files"
            accessibilityRole="button"
            onPress={() => router.push({ pathname: "/files/[paneId]", params: { paneId: toRouteParam(paneId) } })}
            style={styles.iconButton}
          >
            <FolderIcon color={colors.accent} size={24} />
          </Pressable>
        )}
      </View>

      <View style={styles.terminalArea}>
        {gone ? (
          <View style={styles.overlay}>
            <Text style={styles.overlayText}>This terminal no longer exists.</Text>
            <Pressable onPress={() => router.back()} style={styles.button}>
              <Text style={styles.buttonText}>Back</Text>
            </Pressable>
          </View>
        ) : (
          <>
            <TerminalWebView onLoadEnd={() => setPageLoaded(true)} onMessage={onPageMessage} ref={webview} />
            {snapshot.phase === "exited" ? (
              <View style={styles.banner}>
                <Text style={styles.bannerText}>This pane has closed.</Text>
              </View>
            ) : null}
            {snapshot.phase === "noOutput" ? (
              <View pointerEvents="none" style={styles.overlay}>
                <Text style={styles.overlayText}>No output yet. The pane may be idle.</Text>
              </View>
            ) : null}
          </>
        )}
      </View>

      {gone ? null : (
        <>
        <ScrollView contentContainerStyle={styles.chips} horizontal keyboardShouldPersistTaps="always" showsHorizontalScrollIndicator={false} style={styles.chipRow}>
          {KEY_CHIPS.map((chip) => {
            const armed = chip === SHIFT_CHIP && shiftArmed;
            return (
              <Pressable
                accessibilityRole="button"
                accessibilityState={chip === SHIFT_CHIP ? { selected: shiftArmed } : undefined}
                disabled={!inputEnabled}
                key={chip.label}
                onPress={() => onChip(chip)}
                style={({ pressed }) => [styles.chip, armed && styles.chipArmed, pressed && styles.chipPressed, !inputEnabled && styles.disabled]}
              >
                <Text style={[styles.chipLabel, chip.label.length === 1 && styles.chipGlyph, armed && styles.chipArmedLabel]}>{chip.label}</Text>
              </Pressable>
            );
          })}
        </ScrollView>

        <View style={styles.inputBar}>
          <TextInput
            autoCapitalize="none"
            autoCorrect={false}
            editable={inputEnabled}
            onChangeText={setText}
            onSubmitEditing={sendText}
            placeholder="Type, then Send"
            placeholderTextColor={colors.chromeFaint}
            returnKeyType="send"
            style={[styles.input, !inputEnabled && styles.disabled]}
            submitBehavior="submit"
            value={text}
          />
          <Pressable accessibilityRole="button" disabled={!inputEnabled} onPress={sendText} style={[styles.sendButton, !inputEnabled && styles.disabled]}>
            <Text style={styles.sendLabel}>Send</Text>
          </Pressable>
        </View>
        </>
      )}
    </Animated.View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
  header: {
    alignItems: "center",
    backgroundColor: colors.chromeRaised,
    flexDirection: "row",
    gap: 4,
    height: metrics.terminalHeaderHeight,
    paddingHorizontal: 4,
  },
  /** 48 dp touch targets, matching the app bar's Material back arrow. */
  iconButton: { alignItems: "center", height: 48, justifyContent: "center", width: 48 },
  backGlyph: { color: colors.chromeInkStrong, fontSize: 26, fontWeight: "600", lineHeight: 30 },
  title: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.appBarTitle, fontWeight: "600" },
  terminalArea: { backgroundColor: colors.chromeBg, flex: 1 },
  banner: {
    backgroundColor: colors.chromeSelected,
    left: 0,
    paddingHorizontal: 16,
    paddingVertical: 8,
    position: "absolute",
    right: 0,
    top: 0,
  },
  bannerText: { color: colors.chromeInk, fontSize: typeScale.body },
  overlay: {
    alignItems: "center",
    bottom: 0,
    gap: 16,
    justifyContent: "center",
    left: 0,
    position: "absolute",
    right: 0,
    top: 0,
  },
  overlayText: { color: colors.chromeDim, fontSize: typeScale.body, textAlign: "center" },
  button: { backgroundColor: colors.accent, borderRadius: radii.card, paddingHorizontal: 20, paddingVertical: 10 },
  buttonText: { color: colors.accentInk, fontSize: typeScale.body, fontWeight: "600" },
  chipRow: { backgroundColor: colors.chromeRaised, flexGrow: 0, height: metrics.keyChipRowHeight },
  chips: { alignItems: "center", gap: 8, paddingHorizontal: 12 },
  chip: {
    backgroundColor: colors.chromeSelected,
    borderRadius: radii.pill,
    justifyContent: "center",
    minWidth: 44,
    paddingHorizontal: 12,
    height: 28,
  },
  chipPressed: { backgroundColor: colors.chromeBorder },
  /** The ⇧ chip while Shift is armed for the next chip. */
  chipArmed: { backgroundColor: colors.accent },
  chipArmedLabel: { color: colors.accentInk },
  chipLabel: { color: colors.chromeInkStrong, fontSize: typeScale.rowSecondary, textAlign: "center" },
  /** Single-glyph chips (arrows, y, n) read lighter than words at 13 sp; bump them to match. */
  chipGlyph: { fontSize: 16, fontWeight: "600" },
  disabled: { opacity: 0.4 },
  inputBar: {
    alignItems: "center",
    backgroundColor: colors.chromeBg,
    borderTopColor: colors.chromeHairline,
    borderTopWidth: metrics.hairlineWidth,
    flexDirection: "row",
    gap: 8,
    height: metrics.inputBarHeight,
    paddingHorizontal: 12,
  },
  input: {
    backgroundColor: colors.chromeRaised,
    borderRadius: radii.card,
    color: colors.chromeInkStrong,
    flex: 1,
    fontSize: typeScale.body,
    height: 40,
    paddingHorizontal: 12,
  },
  sendButton: { backgroundColor: colors.accent, borderRadius: radii.card, height: 40, justifyContent: "center", paddingHorizontal: 16 },
  sendLabel: { color: colors.accentInk, fontSize: typeScale.body, fontWeight: "600" },
});

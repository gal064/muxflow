import { useFocusEffect, useRouter } from "expo-router";
import * as Clipboard from "expo-clipboard";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Keyboard, Linking, Pressable, StyleSheet, Text, TextInput, View, type TextInputInstance } from "react-native";
import Animated, { KeyboardState, useAnimatedKeyboard, useAnimatedReaction, useAnimatedStyle, useSharedValue } from "react-native-reanimated";
import { runOnJS } from "react-native-worklets";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import type { ChatCommand } from "../../protocol/gen/chat_pb";
import { isCodexAdapter } from "../agents/agentIconPaths";
import { agentForPane, agentStateWord, agentTitle } from "../agents/agentViews";
import { stripAgentStatusGlyphs } from "../agents/agentLabels";
import { markState } from "../agents/agentListModel";
import { AgentHeader, HeaderButton } from "../agents/ui/AgentHeader";
import { attachDeps } from "../chat/attach/attachRuntime";
import { filesRefusal, filesState } from "../chat/attach/attachState";
import { sendWithFiles, useAttachments } from "../chat/attach/useAttachments";
import { slashMatches, withDictation, withPickedCommand } from "../chat/chatModel";
import { AttachSheet } from "../chat/ui/AttachSheet";
import { AttachStrip } from "../chat/ui/AttachStrip";
import { Composer } from "../chat/ui/Composer";
import { notificationAttention } from "../notifications/attention";
import { useVoiceMode, type TypeIntoPane, type VoiceRouting } from "../voice/useVoiceMode";
import { toRouteParam } from "../../navigation/routeParams";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { prefsStore } from "../../store/prefsStore";
import { sessionStore } from "../../store/sessionStore";
import { ConnectionStrip } from "../hosts/ConnectionStrip";
import { FolderIcon } from "../../ui/components/MediaIcons";
import { externalLinkTarget } from "../../ui/externalLinks";
import { useSession } from "../../ui/hooks";
import { lightColors } from "@muxflow/client-core";
import { colors, radii, typeScale } from "../../ui/tokens";
import { displayMessageFor } from "../files/errors";
import { filesStore } from "../files/filesStore";
import { FILE_VIEWER_COPY } from "../files/presentation";
import { resolveTerminalFile, terminalFileCaptureIsCurrent, type TerminalFileCapture } from "../files/terminalFile";
import { appForeground } from "./appForeground";
import type { FromPageMessage } from "./bridgeMessages";
import { pressChip, type KeyChip } from "./chips";
import { KeyChips } from "./KeyChips";
import { terminalKeyboardInset } from "./keyboardInset";
import { TerminalController, type TerminalSnapshot } from "./TerminalController";
import { terminalRegistry } from "./terminalRegistry";
import { paneTerminalScheme } from "./paneTerminalScheme";
import { TerminalWebView, type TerminalWebViewHandle } from "./TerminalWebView";
import { withFilePaths } from "./terminalText";
import { useTerminalCommands } from "./useTerminalCommands";

export interface TerminalScreenProps {
  paneId: string;
  sessionId: string;
  /** Opened from an agent's Talk: start in voice mode. */
  startInVoice?: boolean;
}

const ESC = Uint8Array.of(0x1b);

/** The panes in voice mode, read by their voice controllers, which outlive the screen. */
const voiceModes = new Set<string>();

const KEYBOARD_STATE_NAMES: Record<KeyboardState, string> = {
  [KeyboardState.UNKNOWN]: "unknown",
  [KeyboardState.OPENING]: "opening",
  [KeyboardState.OPEN]: "open",
  [KeyboardState.CLOSING]: "closing",
  [KeyboardState.CLOSED]: "closed",
};

function logKeyboardState(paneId: string, event: "focus" | "blur" | "state", state: KeyboardState, height: number): void {
  log(`[muxflow] ime pane=${paneId} event=${event} state=${KEYBOARD_STATE_NAMES[state] ?? state} height=${Math.round(height)}`);
}

/**
 * Terminal — design.md §9.5. The agent screens' header, the xterm WebView,
 * then the chat's reply field with the key chips over it, or voice mode's
 * talk pane in its place.
 */
export function TerminalScreen({ paneId, sessionId, startInVoice }: TerminalScreenProps) {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  // Expo is edge-to-edge on Android, where adjustResize no longer
  // shrinks the window and RN's Keyboard events under-report the height.
  // Reanimated reads the IME inset itself; padding by it keeps the chips and
  // input bar visible and shrinks the WebView, which re-measures and sends
  // RESIZE_TERMINAL (§7.6 step 6).
  const keyboard = useAnimatedKeyboard();
  const inputFocused = useSharedValue(false);
  const keyboardPadding = useAnimatedStyle(() => ({
    paddingBottom: terminalKeyboardInset(insets.bottom, keyboard.height.value, inputFocused.value),
  }));
  // Observe lifecycle edges only. The reaction reads just `state`, so there is
  // no work during the per-frame height animation and at most four JS log
  // calls per open/close cycle.
  useAnimatedReaction(
    () => keyboard.state.value,
    (state, previous) => {
      if (state !== previous) runOnJS(logKeyboardState)(paneId, "state", state, keyboard.height.value);
    },
  );
  useFocusEffect(useCallback(() => {
    logKeyboardState(paneId, "focus", keyboard.state.value, keyboard.height.value);
    return () => {
      inputFocused.value = false;
      Keyboard.dismiss();
      logKeyboardState(paneId, "blur", keyboard.state.value, keyboard.height.value);
    };
  }, [inputFocused, keyboard.height, keyboard.state, paneId]));
  const state = useSession((s) => s);
  const webview = useRef<TerminalWebViewHandle>(null);
  const controller = useRef<TerminalController | null>(null);
  const screenActive = useRef(false);
  const fileActivation = useRef(0);
  const [snapshot, setSnapshot] = useState<TerminalSnapshot>({ phase: "preparing", grid: undefined, lastError: undefined });
  const [pageLoaded, setPageLoaded] = useState(false);
  const [draft, setDraft] = useState("");
  // §9.5: the ⇧ chip arms Shift for the next chip only; Send also disarms it.
  const [shiftArmed, setShiftArmed] = useState(false);
  const pane = state.panes[paneId];
  const panePresent = pane !== undefined;
  const connected = state.connection.state === "connected";
  // §9.5: the pane left the topology (window closed elsewhere), or never was
  // in it (a retained "gone" agent). New terminal waits for its pane before
  // navigating here, so an absent pane on a loaded topology is conclusive.
  const gone = connected && !pane && state.topologyGeneration > 0n;
  const agent = agentForPane(state, paneId);
  const session = state.sessions[sessionId];
  const window = pane ? state.windows[pane.windowId] : undefined;
  const title = agent
    ? agentTitle(state, agent)
    : `${stripAgentStatusGlyphs(session?.name ?? sessionId)} · ${stripAgentStatusGlyphs(window?.name ?? "")}`.replace(/ · $/, "");
  const agentPresent = agent?.present === true;
  const scheme = paneTerminalScheme(state, paneId);
  const working = agent !== undefined && markState(agent) === "working";
  const agentId = agent?.id;
  useFocusEffect(useCallback(() => (agentId ? notificationAttention.focusAgent(agentId) : undefined), [agentId]));

  // The files on the reply field, typed as paths after its text. Leaving the terminal stops their uploads.
  const files = useAttachments(paneId, attachDeps);
  const [attaching, setAttaching] = useState(false);
  const catalog = useTerminalCommands(paneId, state.serverIdentity, agentPresent ? `${agent.adapterId}:${agent.id}` : undefined, connected);
  // Typed into the agent as is: only Codex reads `$name` as a skill. A chat's host turns it into what its agent takes.
  const dollarSkills = agent !== undefined && isCodexAdapter(agent.adapterId);
  const commands = useMemo(() => slashMatches(catalog, draft, dollarSkills), [catalog, draft, dollarSkills]);
  const pickCommand = useCallback((command: ChatCommand) => setDraft((current) => withPickedCommand(current, command)), []);

  useFocusEffect(useCallback(() => {
    screenActive.current = true;
    return () => {
      screenActive.current = false;
      fileActivation.current += 1;
    };
  }, []));

  useEffect(() => {
    log(`[muxflow] terminal.route pane=${paneId} session=${sessionId} connected=${connected ? "yes" : "no"} panePresent=${panePresent ? "yes" : "no"} decision=${gone ? "unavailable" : "attach"} topology=${sessionStore.getState().topologyGeneration}`);
  }, [connected, gone, paneId, panePresent, sessionId]);

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
      fontSize: prefsStore.getState().terminalFontSize,
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

  const toastError = (error: unknown) => toast(error instanceof Error ? error.message : String(error));

  const openTerminalFile = useCallback(async (candidate: string) => {
    const activation = ++fileActivation.current;
    const connection = getConnection();
    const capturedState = sessionStore.getState();
    const capturedPane = capturedState.panes[paneId];
    if (!connection || capturedState.connection.state !== "connected" || !capturedPane
      || connection.serverIdentity !== capturedState.serverIdentity) {
      toast("Reconnect the terminal before opening a file path.");
      return;
    }
    const capture: TerminalFileCapture = {
      pane: {
        id: capturedPane.id,
        sessionId: capturedPane.sessionId,
        windowId: capturedPane.windowId,
        currentPath: capturedPane.currentPath,
      },
      serverIdentity: capturedState.serverIdentity,
      topologyGeneration: capturedState.topologyGeneration,
    };
    const connectionEpoch = connection.connectionEpoch;
    try {
      const resolved = await resolveTerminalFile(connection.request.bind(connection), candidate, capture);
      if (!screenActive.current || !appForeground.inForeground() || activation !== fileActivation.current) return;
      const liveConnection = getConnection();
      if (liveConnection !== connection || liveConnection.connectionEpoch !== connectionEpoch) return;
      if (!terminalFileCaptureIsCurrent(capture, sessionStore.getState(), resolved.topologyGeneration)) return;
      filesStore.getState().setTerminalFileRoot(resolved.path, resolved.root);
      router.push({
        pathname: "/file/[paneId]",
        params: {
          paneId: toRouteParam(paneId),
          path: toRouteParam(resolved.path),
          name: toRouteParam(resolved.name),
        },
      });
    } catch (error) {
      if (screenActive.current && appForeground.inForeground() && activation === fileActivation.current) {
        toast(FILE_VIEWER_COPY.error(displayMessageFor(error)));
      }
    }
  }, [paneId, router]);

  const onPageMessage = useCallback((message: FromPageMessage) => {
    switch (message.t) {
      case "copy":
        fileActivation.current += 1;
        Clipboard.setStringAsync(message.text).catch(() => toast("Couldn't copy the terminal selection."));
        return;
      case "openLink": {
        fileActivation.current += 1;
        const target = externalLinkTarget(message.href);
        if (!target || !/^https?:/iu.test(target)) return;
        Linking.openURL(target).catch(() => toast("Couldn't open the link."));
        return;
      }
      case "openFile":
        void openTerminalFile(message.path);
        return;
      default:
        controller.current?.onPageMessage(message);
    }
  }, [openTerminalFile]);

  const send = useCallback((bytes: Uint8Array) => {
    const instance = controller.current;
    if (!instance) return;
    instance.sendInput(bytes).catch(toastError);
  }, []);

  // Clear the field before the submit round trip so a second Send cannot
  // resend the same text; the controller pastes the body, then sends the CR.
  // The files leave the field with it, and come back if it was refused.
  const sendText = useCallback(() => {
    const instance = controller.current;
    if (!instance) return;
    const held = filesRefusal(filesState(files.entries));
    if (held !== undefined) {
      toast(held);
      return;
    }
    const body = draft;
    setDraft("");
    setShiftArmed(false);
    sendWithFiles(files, (attached) => instance.submitText(withFilePaths(body, attached))).catch((error: unknown) => {
      setDraft((current) => current || body);
      toastError(error);
    });
  }, [draft, files]);

  // A dictation is what was typed with what was said after it, typed as
  // Send types it. While the files cannot go yet it waits in the field.
  const dictate = useCallback(async (spoken: string, typeIntoPane: TypeIntoPane) => {
    const text = withDictation(draft, spoken);
    const held = filesRefusal(filesState(files.entries));
    if (held !== undefined) {
      setDraft(text);
      toast(held);
      return;
    }
    setDraft("");
    let sent = false;
    try {
      await sendWithFiles(files, async (attached) => {
        sent = await typeIntoPane(withFilePaths(text, attached));
      });
    } catch (error) {
      // As Send does: what was typed and said comes back to the field.
      setDraft((current) => current || text);
      throw error;
    }
    return sent;
  }, [draft, files]);

  const [voiceMode, setVoiceMode] = useState(startInVoice === true);
  useEffect(() => {
    if (!voiceMode) return undefined;
    voiceModes.add(paneId);
    return () => {
      voiceModes.delete(paneId);
    };
  }, [paneId, voiceMode]);
  // Voice mode types what was said into the agent, as does a dictation that
  // came back after the terminal was left.
  const routing = useMemo((): VoiceRouting => ({
    chat: false,
    inVoiceMode: () => voiceModes.has(paneId),
    send: (text, typeIntoPane) => typeIntoPane(text),
    orphan: (text, typeIntoPane) => typeIntoPane(text),
    canSubmit: (target) => agentForPane(sessionStore.getState(), target)?.present === true,
  }), [paneId]);
  // Stop is the agent's interrupt key, as the Esc chip sends it.
  const stop = useCallback(() => send(ESC), [send]);
  const leaveVoiceMode = useCallback(() => setVoiceMode(false), []);
  const voice = useVoiceMode({
    paneId,
    sessionId,
    serverIdentity: state.serverIdentity,
    voiceMode,
    connected,
    gone: !agentPresent,
    screenHint: "",
    working,
    stopping: false,
    onStop: stop,
    onKeyboard: leaveVoiceMode,
    dictate,
    routing,
  });
  // The agent picking up what was said is a haptic, as the working line is not in view.
  const lifecycle = agent?.lifecycle;
  const voiceController = voice.controller;
  useEffect(() => {
    if (voiceMode && lifecycle) voiceController?.onAgentLifecycle(lifecycle);
  }, [lifecycle, voiceController, voiceMode]);
  // Once the agent has left, voice mode stays until it is left, its pane
  // saying why it takes nothing, so a take on its way can still be ended.
  const voiceParts = agentPresent || voiceMode ? voice.parts : undefined;
  const voicePane = voiceMode && voiceParts?.ready === true;
  // The talk pane's keyboard button leaves voice mode for the reply field, so the field takes the focus.
  const field = useRef<TextInputInstance>(null);
  const wasVoicePane = useRef(voicePane);
  useEffect(() => {
    if (wasVoicePane.current && !voiceMode) field.current?.focus();
    wasVoicePane.current = voicePane;
  }, [voiceMode, voicePane]);

  const onChip = useCallback((chip: KeyChip) => {
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
      <AgentHeader
        line={agent ? agentStateWord(agent) : undefined}
        mark={agent ? { adapterId: agent.adapterId, state: markState(agent) } : undefined}
        onBack={() => router.back()}
        title={title}
      >
        {gone ? null : (
          <HeaderButton label="Files" onPress={() => router.push({ pathname: "/files/[paneId]", params: { paneId: toRouteParam(paneId) } })}>
            <FolderIcon color={colors.chromeDim} size={20} />
          </HeaderButton>
        )}
      </AgentHeader>

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
            <TerminalWebView onLoadEnd={() => setPageLoaded(true)} onMessage={onPageMessage} ref={webview} scheme={scheme} />
            {snapshot.phase === "exited" ? (
              <View style={styles.banner}>
                <Text style={styles.bannerText}>This pane has closed.</Text>
              </View>
            ) : null}
            {snapshot.phase === "noOutput" ? (
              <View pointerEvents="none" style={styles.overlay}>
                {/* Drawn over the terminal itself, so it takes the terminal's light or dark ink. */}
                <Text style={[styles.overlayText, scheme === "light" ? { color: lightColors.chromeDim } : null]}>No output yet. The pane may be idle.</Text>
              </View>
            ) : null}
          </>
        )}
      </View>

      {gone ? null : (
        <>
          {/* The reply being read sits under the terminal, which cannot hold a player. */}
          {voiceMode && voiceParts?.reply ? <View style={styles.reply}>{voiceParts.reply}</View> : null}
          {/* The setup card shows until voice is ready on the host, and the reply field stays under it until then. */}
          {voiceMode && voiceParts ? voiceParts.status : null}
          {voicePane && voiceParts ? voiceParts.pane : (
            <Composer
              above={<KeyChips disabled={!inputEnabled} onChip={onChip} shiftArmed={shiftArmed} />}
              attach={{
                files: filesState(files.entries),
                onOpen: () => {
                  Keyboard.dismiss();
                  setAttaching(true);
                },
                strip: <AttachStrip entries={files.entries} onRemove={files.remove} />,
              }}
              canSendMidTurn
              commands={commands}
              disabled={!inputEnabled}
              draft={draft}
              editing={false}
              inputRef={field}
              inputTestID="terminal-input"
              mic={agentPresent || voiceMode ? {
                on: voiceMode,
                // In voice mode before voice is ready (not set up, not connected) the mic is the way out.
                onTap: () => {
                  if (!voiceMode) Keyboard.dismiss();
                  setVoiceMode(!voiceMode);
                },
                dictation: voiceParts?.dictation,
              } : undefined}
              onBlur={() => { inputFocused.value = false; }}
              onCancelEdit={() => undefined}
              onDraft={setDraft}
              onFocus={() => { inputFocused.value = true; }}
              onPickCommand={pickCommand}
              onSend={sendText}
              onStop={stop}
              // A shell is typed to as is; an agent is written to as the chat is.
              placeholder={agent ? undefined : "Type, then Send"}
              plain={!agent}
              sending={false}
              sideQuestion={false}
              waiting={false}
              working={working}
            />
          )}
        </>
      )}
      <AttachSheet onChoose={files.choose} onDismiss={() => setAttaching(false)} visible={attaching} />
    </Animated.View>
  );
}

const styles = StyleSheet.create({
  root: { backgroundColor: colors.chromeBg, flex: 1 },
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
  reply: { paddingHorizontal: 10, paddingTop: 8 },
});

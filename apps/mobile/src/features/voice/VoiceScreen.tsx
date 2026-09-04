import { useFocusEffect, useRouter } from "expo-router";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { AppState, Pressable, ScrollView, StyleSheet, Text, useWindowDimensions, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { useStore } from "zustand";

import { agentStateLabel, agentTitle } from "../agents/agentViews";
import { markState } from "../agents/agentListModel";
import { AgentMark } from "../agents/ui/AgentMark";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { ConnectionStrip } from "../hosts/ConnectionStrip";
import { prefsStore } from "../../store/prefsStore";
import { sessionStore } from "../../store/sessionStore";
import { Dialog } from "../../ui/components/Dialog";
import { EmptyState } from "../../ui/components/EmptyState";
import { BackIcon } from "../../ui/components/MediaIcons";
import { useSession } from "../../ui/hooks";
import { colors, fixedChromeText, metrics, radii, typeScale } from "../../ui/tokens";
import { useAnimationsAllowed } from "../../ui/useAnimationsAllowed";
import { createExpoFiles } from "./files";
import { createExpoHaptics } from "./haptics";
import { MicButton } from "./MicButton";
import { bigPaneHeight } from "./paneLayout";
import { createExpoPlayer } from "./player";
import { createExpoRecorder } from "./recorder";
import { ReplyPlayer } from "./ReplyPlayer";
import { SpeedPicker } from "./SpeedPicker";
import { createExpoTones } from "./tones";
import type { VoiceController } from "./VoiceController";
import { useVoice } from "./voiceHooks";
import { voiceRegistry } from "./voiceRegistry";
import { WorkingIndicator } from "./WorkingIndicator";
import { VoiceStatusCard } from "./VoiceStatusCard";
import { latestReply, type VoiceMessage } from "./voiceStore";

export interface VoiceScreenProps {
  agentId: string;
  paneId: string;
  sessionId: string;
}

// The one recorder, player, file and haptics adapter for the app, created when
// the first Voice screen renders (§2c: nothing audio-related loads before that).
let audio: { recorder: ReturnType<typeof createExpoRecorder>; player: ReturnType<typeof createExpoPlayer>; files: ReturnType<typeof createExpoFiles>; haptics: ReturnType<typeof createExpoHaptics>; tones: ReturnType<typeof createExpoTones> } | undefined;
function sharedAudio() {
  audio ??= { recorder: createExpoRecorder(), player: createExpoPlayer(), files: createExpoFiles(), haptics: createExpoHaptics(), tones: createExpoTones() };
  return audio;
}

/** Voice — design.md §9.11. Header, message list, readiness card, the talk pane. */
export function VoiceScreen({ agentId, paneId, sessionId }: VoiceScreenProps) {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const animateAgentState = useAnimationsAllowed();
  const { height: windowHeight } = useWindowDimensions();
  const agent = useSession((s) => s.agents[agentId]);
  const lifecycle = agent?.lifecycle;
  const windows = useSession((s) => s.windows);
  const adapters = useSession((s) => s.adapters);
  const connected = useSession((s) => s.connection.state === "connected");
  const readiness = useVoice((s) => s.hostStatus.readiness);
  const recorderError = useVoice((s) => s.recorderError);
  const session = useVoice((s) => s.sessions[agentId]);
  const playbackRate = useStore(prefsStore, (s) => s.voicePlaybackRate);
  const bigPane = useStore(prefsStore, (s) => s.voiceBigPane);
  const [confirmEnd, setConfirmEnd] = useState(false);
  const list = useRef<ScrollView>(null);

  const open = useCallback((): VoiceController => voiceRegistry.open({
    agentId,
    paneId,
    sessionId,
    getConnection,
    ...sharedAudio(),
    appInForeground: () => AppState.currentState === "active",
    canSubmit: () => sessionStore.getState().agents[agentId]?.present === true,
    playbackRate: prefsStore.getState().voicePlaybackRate,
    toast,
    log,
  }), [agentId, paneId, sessionId]);
  const [controller, setController] = useState(open);
  // Set by End session: the screen is popping and must not resurrect the session it just ended.
  const ending = useRef(false);

  // A disconnect from the strip on this very screen disposes the session it was
  // built on; the reconnect that follows (or the next focus) starts a fresh one.
  useEffect(() => {
    if (connected && controller.isDisposed && !ending.current) setController(open());
  }, [connected, controller, open]);
  useFocusEffect(useCallback(() => {
    const live = controller.isDisposed ? open() : controller;
    if (live !== controller) setController(live);
    live.focus();
    return () => live.blur();
  }, [controller, open]));

  useEffect(() => {
    const subscription = AppState.addEventListener("change", (next) => {
      if (next === "active") controller.onAppActive();
    });
    return () => subscription.remove();
  }, [controller]);

  // The speed applies to the reply that is playing as well as the next one.
  useEffect(() => controller.setPlaybackRate(playbackRate), [controller, playbackRate]);
  // The agent picking up the utterance is a haptic, not only the working bubble.
  useEffect(() => {
    if (lifecycle) controller.onAgentLifecycle(lifecycle);
  }, [controller, lifecycle]);

  const messages = session?.messages ?? [];
  const phase = session?.phase ?? "idle";
  const newest = latestReply(session);

  const title = agent ? agentTitle({ windows, adapters }, agent) : "Agent";
  // The pane may survive an exited agent as an ordinary shell. Never submit
  // dictated text there after the host has confirmed the agent's departure.
  const gone = !agent || !agent.present;
  // One phrase per condition, matching the readiness card above the mic.
  const hint = !connected
    ? "Not connected"
    : gone
      ? "This agent no longer exists"
      : readiness === "unknown"
        ? "Checking voice on the host…"
        : readiness === "uvMissing"
          ? "Needs uv on the host"
          : readiness === "provisioning"
            ? "Setting up voice on the host…"
            : readiness === "modelMissing"
              ? "Voice isn't set up on this host"
              : recorderError
                ? "Microphone unavailable: allow the microphone permission for Muxflow"
                : "";
  const micDisabled = hint !== "";

  const endSession = useCallback(() => {
    setConfirmEnd(false);
    ending.current = true;
    void voiceRegistry.end(agentId);
    router.back();
  }, [agentId, router]);

  const scrollToEnd = useCallback((animated: boolean) => list.current?.scrollToEnd({ animated }), []);

  return (
    <View style={[styles.root, { paddingBottom: insets.bottom, paddingTop: insets.top }]}>
      <ConnectionStrip />
      <View style={styles.header}>
        <Pressable accessibilityLabel="Back" accessibilityRole="button" onPress={() => router.back()} style={styles.iconButton}>
          <BackIcon color={colors.chromeInkStrong} />
        </Pressable>
        {agent ? <AgentMark adapterId={agent.adapterId} animate={animateAgentState} ring={colors.chromeRaised} state={markState(agent)} surface={colors.chromeBg} /> : null}
        <Text {...fixedChromeText} accessibilityLabel={agent ? `${title}, ${agentStateLabel(agent)}` : title} accessibilityRole="header" numberOfLines={1} style={styles.title}>{title}</Text>
        <Pressable accessibilityLabel="End session" accessibilityRole="button" onPress={() => setConfirmEnd(true)} style={styles.endButton}>
          <Text {...fixedChromeText} numberOfLines={1} style={styles.endLabel}>End</Text>
        </Pressable>
      </View>

      {/* The list re-pins to its end when it grows and when the pane below changes size, so the newest turn stays in view. */}
      <ScrollView contentContainerStyle={styles.listContent} onContentSizeChange={() => scrollToEnd(true)} onLayout={() => scrollToEnd(false)} ref={list} style={styles.list}>
        {messages.length === 0 ? (
          <EmptyState heading={`Talk to ${title}`} lines={["Hold anywhere on the pane below, say what you want typed into the agent's terminal, and let go.", "The agent's next reply is read back to you here."]} />
        ) : null}
        {messages.map((message) => (
          <MessageBubble controller={message.id === newest?.id ? controller : undefined} key={message.id} message={message} />
        ))}
        <WorkingIndicator agentId={agentId} />
      </ScrollView>

      <VoiceStatusCard connected={connected} controller={controller} />

      <View style={[styles.pane, bigPane && { height: bigPaneHeight(windowHeight, insets.top, insets.bottom, metrics.terminalHeaderHeight) }]}>
        <View style={styles.paneControls}>
          <View style={styles.speedGroup}>
            <Text {...fixedChromeText} numberOfLines={1} style={styles.speedCaption}>Speed</Text>
            <SpeedPicker onChange={(rate) => prefsStore.getState().setVoicePlaybackRate(rate)} rate={playbackRate} />
          </View>
          <Pressable
            accessibilityLabel={bigPane ? "Smaller talk pane" : "Larger talk pane"}
            accessibilityRole="button"
            accessibilityState={{ selected: bigPane }}
            hitSlop={{ top: 6, bottom: 6 }}
            onPress={() => prefsStore.getState().setVoiceBigPane(!bigPane)}
            style={({ pressed }) => [styles.paneToggle, pressed && styles.pressed]}
          >
            <Text {...fixedChromeText} numberOfLines={1} style={styles.paneToggleLabel}>{bigPane ? "Smaller" : "Bigger"}</Text>
          </Pressable>
        </View>
        <MicButton
          disabled={micDisabled}
          hint={hint}
          onPressIn={() => controller.beginUtterance()}
          onPressOut={() => void controller.endUtterance()}
          phase={phase}
        />
      </View>

      <Dialog
        actions={[
          { label: "Keep", onPress: () => setConfirmEnd(false) },
          { label: "End session", onPress: endSession, variant: "danger" },
        ]}
        message="The conversation and the last reply's audio are removed from this phone. The agent keeps running."
        onDismiss={() => setConfirmEnd(false)}
        title="End this voice session?"
        visible={confirmEnd}
      />
    </View>
  );
}

/** One turn. Agent replies collapse to three lines; tap to expand. The newest reply carries the player. */
const MessageBubble = memo(function MessageBubble({ message, controller }: { message: VoiceMessage; controller: VoiceController | undefined }) {
  const [expanded, setExpanded] = useState(false);
  const you = message.kind === "you";
  const stamp = new Date(message.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return (
    <View style={[styles.bubbleRow, you && styles.bubbleRowYou]}>
      {/* A bubble sizes to its text; one that carries the player takes the row, or the player's flex track collapses to a stub. */}
      <View style={[styles.bubble, you ? styles.bubbleYou : styles.bubbleAgent, controller ? styles.bubbleWithPlayer : null]}>
        {/* The text is the tap target; the player below stays its own set of controls for a screen reader. */}
        <Pressable
          accessibilityHint={you ? undefined : expanded ? "Collapses the reply" : "Expands the reply"}
          accessibilityLabel={`${you ? "You" : "Agent"}: ${message.text}`}
          accessibilityRole={you ? "text" : "button"}
          disabled={you}
          hitSlop={you ? undefined : { top: 10, bottom: 10 }}
          onPress={() => setExpanded((value) => !value)}
          style={you ? undefined : styles.expandTarget}
        >
          <Text numberOfLines={you || expanded ? undefined : 3} style={[styles.bubbleText, you && styles.bubbleTextYou]}>{message.text}</Text>
          {message.truncated && expanded ? <Text style={styles.truncatedNote}>Spoken reply shortened; the rest is in the terminal.</Text> : null}
        </Pressable>
        {controller ? <ReplyPlayer controller={controller} message={message} /> : null}
        <Text style={[styles.stamp, you && styles.stampYou]}>{stamp}</Text>
      </View>
    </View>
  );
});


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
  /** 48 dp touch targets, matching the Terminal header. */
  iconButton: { alignItems: "center", height: 48, justifyContent: "center", width: 48 },
  title: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.appBarTitle, fontWeight: "600", minWidth: 0 },
  endButton: { alignItems: "center", height: 48, justifyContent: "center", paddingHorizontal: 12 },
  endLabel: { color: colors.dangerInk, fontSize: typeScale.body, fontWeight: "600" },
  list: { flex: 1 },
  listContent: { flexGrow: 1, gap: 8, paddingHorizontal: 12, paddingVertical: 12 },
  bubbleRow: { flexDirection: "row", justifyContent: "flex-start" },
  bubbleRowYou: { justifyContent: "flex-end" },
  bubble: { borderRadius: radii.card, borderWidth: metrics.hairlineWidth, gap: 4, maxWidth: "88%", paddingHorizontal: 14, paddingVertical: 10 },
  // `--chrome-raised` on `--chrome-bg` is a 1.07:1 step; the hairline is what separates a bubble from the page.
  bubbleAgent: { backgroundColor: colors.chromeRaised, borderColor: colors.chromeBorder },
  bubbleYou: { backgroundColor: colors.accentWash, borderColor: colors.accent },
  bubbleWithPlayer: { width: "88%" },
  // With the 10 dp slop this makes a one-line reply a 48 dp expand target.
  expandTarget: { minHeight: 28 },
  bubbleText: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20 },
  bubbleTextYou: { color: colors.chromeInkStrong },
  truncatedNote: { color: colors.chromeDim, fontSize: typeScale.meta, fontStyle: "italic" },
  stamp: { color: colors.chromeDim, fontSize: typeScale.meta },
  stampYou: { textAlign: "right" },
  /** The talk pane: a hairline above, the controls row, then the hold surface filling the rest. */
  pane: { borderTopColor: colors.chromeBorder, borderTopWidth: metrics.hairlineWidth },
  paneControls: { alignItems: "center", flexDirection: "row", justifyContent: "space-between", paddingBottom: 8, paddingHorizontal: 12, paddingTop: 8 },
  speedGroup: { alignItems: "center", flexDirection: "row", gap: 8 },
  speedCaption: { color: colors.chromeDim, fontSize: typeScale.rowSecondary },
  /** Wide enough for "Smaller", so the pill's left edge does not jump when the label changes. */
  paneToggle: { alignItems: "center", backgroundColor: colors.chromeRaised, borderRadius: radii.card, height: 40, justifyContent: "center", minWidth: 96, paddingHorizontal: 14 },
  paneToggleLabel: { color: colors.chromeInk, fontSize: typeScale.rowSecondary, fontWeight: "600" },
  pressed: { opacity: 0.75 },
});

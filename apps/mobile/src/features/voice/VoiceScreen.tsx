import { useFocusEffect, useRouter } from "expo-router";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { AppState, Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { agentPillState } from "../agents/agentViews";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { ConnectionStrip } from "../hosts/ConnectionStrip";
import { Dialog } from "../../ui/components/Dialog";
import { EmptyState } from "../../ui/components/EmptyState";
import { StatusPill } from "../../ui/components/StatusPill";
import { useSession } from "../../ui/hooks";
import { colors, metrics, radii, typeScale } from "../../ui/tokens";
import { createExpoFiles } from "./files";
import { MicButton } from "./MicButton";
import { createExpoPlayer } from "./player";
import { createExpoRecorder } from "./recorder";
import { ReplyPlayer } from "./ReplyPlayer";
import type { VoiceController } from "./VoiceController";
import { useVoice } from "./voiceHooks";
import { voiceRegistry } from "./voiceRegistry";
import { VoiceStatusCard } from "./VoiceStatusCard";
import { latestReply, type VoiceMessage } from "./voiceStore";

export interface VoiceScreenProps {
  agentId: string;
  paneId: string;
  sessionId: string;
}

// The one recorder, player and file adapter for the app, created when the
// first Voice screen renders (§2c: nothing audio-related loads before that).
let audio: { recorder: ReturnType<typeof createExpoRecorder>; player: ReturnType<typeof createExpoPlayer>; files: ReturnType<typeof createExpoFiles> } | undefined;
function sharedAudio() {
  audio ??= { recorder: createExpoRecorder(), player: createExpoPlayer(), files: createExpoFiles() };
  return audio;
}

/** Voice — design.md §9.11. Header, message list, readiness card, hold-to-talk. */
export function VoiceScreen({ agentId, paneId, sessionId }: VoiceScreenProps) {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const agent = useSession((s) => s.agents[agentId]);
  const connected = useSession((s) => s.connection.state === "connected");
  const readiness = useVoice((s) => s.hostStatus.readiness);
  const recorderError = useVoice((s) => s.recorderError);
  const session = useVoice((s) => s.sessions[agentId]);
  const [confirmEnd, setConfirmEnd] = useState(false);
  const list = useRef<ScrollView>(null);

  const open = useCallback((): VoiceController => voiceRegistry.open({
    agentId,
    paneId,
    sessionId,
    getConnection,
    ...sharedAudio(),
    appInForeground: () => AppState.currentState === "active",
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

  const messages = session?.messages ?? [];
  const phase = session?.phase ?? "idle";
  const newest = latestReply(session);

  const title = agent?.displayName || "Agent";
  const gone = agent !== undefined && !agent.present;
  // One phrase per condition, matching the readiness card above the mic.
  const hint = !connected
    ? "Not connected"
    : !agent || gone
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

  return (
    <View style={[styles.root, { paddingBottom: insets.bottom, paddingTop: insets.top }]}>
      <ConnectionStrip />
      <View style={styles.header}>
        <Pressable accessibilityLabel="Back" accessibilityRole="button" onPress={() => router.back()} style={styles.iconButton}>
          <Text style={styles.backGlyph}>←</Text>
        </Pressable>
        <Text accessibilityRole="header" numberOfLines={1} style={styles.title}>{title}</Text>
        {agent ? <StatusPill state={agentPillState(agent)} /> : null}
        <Pressable accessibilityLabel="End session" accessibilityRole="button" onPress={() => setConfirmEnd(true)} style={styles.endButton}>
          <Text style={styles.endLabel}>End</Text>
        </Pressable>
      </View>

      <ScrollView contentContainerStyle={styles.listContent} onContentSizeChange={() => list.current?.scrollToEnd({ animated: true })} ref={list} style={styles.list}>
        {messages.length === 0 ? (
          <EmptyState heading={`Talk to ${title}`} lines={["Hold the button, say what you want typed into the agent's terminal, and let go.", "The agent's next reply is read back to you here."]} />
        ) : null}
        {messages.map((message) => (
          <MessageBubble controller={message.id === newest?.id ? controller : undefined} key={message.id} message={message} />
        ))}
      </ScrollView>

      <VoiceStatusCard connected={connected} controller={controller} />

      <MicButton
        disabled={micDisabled}
        hint={hint}
        onPressIn={() => controller.beginUtterance()}
        onPressOut={() => void controller.endUtterance()}
        phase={phase}
      />

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
      <View style={[styles.bubble, you ? styles.bubbleYou : styles.bubbleAgent]}>
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
  backGlyph: { color: colors.chromeInkStrong, fontSize: 26, fontWeight: "600", lineHeight: 30 },
  title: { color: colors.chromeInkStrong, flex: 1, fontSize: typeScale.appBarTitle, fontWeight: "600" },
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
  // With the 10 dp slop this makes a one-line reply a 48 dp expand target.
  expandTarget: { minHeight: 28 },
  bubbleText: { color: colors.chromeInk, fontSize: typeScale.body, lineHeight: 20 },
  bubbleTextYou: { color: colors.chromeInkStrong },
  truncatedNote: { color: colors.chromeDim, fontSize: typeScale.meta, fontStyle: "italic" },
  stamp: { color: colors.chromeDim, fontSize: typeScale.meta },
  stampYou: { textAlign: "right" },
});

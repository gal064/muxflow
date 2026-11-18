// Voice mode inside a chat (chat-mock.html, "Voice mode, inside the chat"):
// once voice is set up on the host, the composer becomes the hold-to-talk
// bar, a transcript is sent as a chat message, and each finished reply is
// read aloud with VOICE_SPEAK. The same `VoiceController` as the Voice screen
// does the recording, transcription and playback; it registers nothing with
// the host, so no reply is pushed twice.

import { useFocusEffect } from "expo-router";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { AppState, Platform } from "react-native";
import { useStore } from "zustand";

import { ChatConnection, ChatTurnState } from "../../protocol/gen/chat_pb";
import { androidConnectionServices } from "../../session/AndroidConnectionServices";
import { backgroundTimer } from "../../session/backgroundTimer";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { muxflowSsh } from "../../ssh/MuxflowSsh";
import { prefsStore } from "../../store/prefsStore";
import { sharedAudio } from "../voice/sharedAudio";
import type { VoiceController } from "../voice/VoiceController";
import { useVoice } from "../voice/voiceHooks";
import { voiceHint } from "../voice/voiceHint";
import { voiceRegistry } from "../voice/voiceRegistry";
import { VoiceStatusCard } from "../voice/VoiceStatusCard";
import { latestReply } from "../voice/voiceStore";
import { replyToSpeak, turnOf, type ChatPane } from "./chatModel";
import { chatClient } from "./chatRuntime";
import { ChatReplyRow, ChatVoiceBar } from "./ui/ChatVoiceParts";

export interface ChatVoiceParts {
  status: ReactNode;
  reply: ReactNode;
  bar: ReactNode;
  /** Voice is set up on the host, so the bar can take the reply field's place. Until then the field stays, under the status card. */
  ready: boolean;
}

export function useChatVoice({ paneId, sessionId, serverIdentity, pane, enabled, connected }: {
  paneId: string;
  sessionId: string;
  serverIdentity: string;
  pane: ChatPane | undefined;
  enabled: boolean;
  connected: boolean;
}): ChatVoiceParts | undefined {
  const open = useCallback((): VoiceController => {
    // A Voice screen opened before the pane was known to be a chat made a
    // terminal controller (it types into the pane and registers with the
    // host). Voice mode needs its own.
    const existing = voiceRegistry.get(paneId);
    if (existing && !existing.submitsToChat) void voiceRegistry.end(paneId);
    return voiceRegistry.open({
      serverIdentity,
      paneId,
      sessionId,
      getConnection,
      ...sharedAudio(),
      appInForeground: () => AppState.currentState === "active",
      canPlayScreenOff: () => Platform.OS === "android" && prefsStore.getState().voicePlayScreenOff && androidConnectionServices().isScreenOffOrLocked(),
      backgroundTimer: backgroundTimer(),
      canSubmit: (target) => chatClient.pane(target)?.status === "open",
      keepAlive: () => muxflowSsh().keepAlive(),
      playbackRate: prefsStore.getState().voicePlaybackRate,
      // Voice mode is reading replies aloud: it plays them whatever the Voice screen's preference.
      autoPlay: true,
      submit: (text) => chatClient.send(paneId, text),
      registerWithHost: false,
      toast,
      log,
    });
  }, [paneId, serverIdentity, sessionId]);
  // The registry keeps one controller per pane. It outlives a reconnect, so
  // a brief drop does not cancel a take or stop a reply (as on the Voice
  // screen); one disposed meanwhile (host forgotten, server replaced) is
  // replaced once connected.
  const [controller, setController] = useState<VoiceController | undefined>(undefined);
  useEffect(() => {
    if (!enabled) setController(undefined);
    else if ((!controller || controller.isDisposed) && connected) setController(open());
  }, [connected, controller, enabled, open]);
  const playbackRate = useStore(prefsStore, (s) => s.voicePlaybackRate);

  useFocusEffect(useCallback(() => {
    if (!controller || controller.isDisposed) return;
    const live = controller;
    live.focus();
    const subscription = AppState.addEventListener("change", (next) => {
      if (next === "active") live.onAppActive();
      else live.onAppInactive();
      if (next === "background") live.onAppBackground();
    });
    return () => {
      subscription.remove();
      live.blur();
    };
  }, [controller]));
  useEffect(() => controller?.setPlaybackRate(playbackRate), [controller, playbackRate]);

  // A turn that ends is read aloud: its newest reply, once.
  const previousTurn = useRef(turnOf(pane));
  const lastSpoken = useRef<string | undefined>(undefined);
  useEffect(() => {
    const before = previousTurn.current;
    previousTurn.current = turnOf(pane);
    if (!enabled || !controller || controller.isDisposed) return;
    const reply = replyToSpeak(before, pane, lastSpoken.current);
    if (!reply) return;
    lastSpoken.current = reply.id;
    void controller.speakReply(reply.text, Number(reply.atUnixMillis) || Date.now());
  }, [controller, enabled, pane]);

  const sessionKey = controller?.sessionKey ?? "";
  const phase = useVoice((s) => s.sessions[sessionKey]?.phase ?? "idle");
  const reply = useVoice((s) => latestReply(s.sessions[sessionKey]));
  const readiness = useVoice((s) => s.hostStatus.readiness);
  const recorderError = useVoice((s) => s.recorderError);
  if (!enabled || !controller) return undefined;
  const gone = pane?.status === "gone";
  const hint = (connected && !gone ? chatHint(pane) : "") || voiceHint({ connected, gone, readiness, recorderError });
  return {
    status: <VoiceStatusCard connected={connected} controller={controller} />,
    reply: reply ? <ChatReplyRow controller={controller} message={reply} playbackRate={playbackRate} /> : null,
    bar: <ChatVoiceBar controller={controller} hint={hint} onStop={() => void chatClient.interrupt(paneId).catch(() => undefined)} phase={phase} working={turnOf(pane) === ChatTurnState.WORKING} />,
    ready: readiness === "ready",
  };
}

/** Why a chat that is still on screen takes no message now; empty when it does. */
function chatHint(pane: ChatPane | undefined): string {
  if (pane?.state?.connection === ChatConnection.ENDED || pane?.state?.connection === ChatConnection.FAILED) return "The agent has stopped";
  if (!pane || pane.status !== "open" || pane.resyncing) return "Waiting for the chat…";
  return "";
}

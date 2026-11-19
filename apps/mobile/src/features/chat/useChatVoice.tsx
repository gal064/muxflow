// Voice in a chat (mobile-voice-mock.html). The reply field's mic does two
// things: a hold dictates one message, sent as Send would send it with what is
// typed and attached; a tap turns voice mode on, where the talk pane takes the
// reply field's place and each finished reply is read aloud with VOICE_SPEAK.
// One `VoiceController` per pane does the recording, transcription and
// playback for both; it registers nothing with the host, so no reply is pushed
// twice.
//
// The microphone is only made ready while it can be used: in voice mode, or
// from the moment a finger is on the mic until its take is sent. Readying it
// takes the audio session (other apps duck, a headset moves to its call
// profile) and asks for the permission the first time, which opening a chat
// must not do.

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
import { replyToSpeak, turnOf, withDictation, type ChatPane } from "./chatModel";
import { chatClient, chatStore } from "./chatRuntime";
import { readDraft, writeDraft } from "./drafts";
import type { ComposerDictation } from "./ui/ComposerMic";
import { ChatReplyRow, ChatVoicePane } from "./ui/ChatVoiceParts";

export interface ChatVoiceParts {
  /** Voice mode: the setup card, until voice is ready on the host. */
  status: ReactNode;
  /** Voice mode: the newest reply's player, the conversation's last row. */
  reply: ReactNode;
  /** Voice mode: the talk pane, in the reply field's place once `ready`. */
  pane: ReactNode;
  /** Voice is set up on the host. */
  ready: boolean;
  /** The reply field's hold to dictate. */
  dictation: ComposerDictation;
}

/**
 * Where a dictated transcript goes, per pane: the chat screen's send, with the
 * draft and the files. Kept here rather than in the controller's options
 * because the registry's controller outlives the screen that made it.
 */
const dictationSends = new Map<string, (text: string) => Promise<void>>();

export function useChatVoice({ paneId, sessionId, serverIdentity, pane, voiceMode, connected, stopping, onStop, onKeyboard, dictate }: {
  paneId: string;
  sessionId: string;
  serverIdentity: string;
  pane: ChatPane | undefined;
  voiceMode: boolean;
  connected: boolean;
  /** Stop was pressed and the turn has not stopped yet. */
  stopping: boolean;
  onStop: () => void;
  /** Leaves voice mode for the reply field. */
  onKeyboard: () => void;
  /** Sends a dictated transcript as Send would. */
  dictate: (text: string) => Promise<void>;
}): ChatVoiceParts | undefined {
  useEffect(() => {
    dictationSends.set(paneId, dictate);
    return () => {
      if (dictationSends.get(paneId) === dictate) dictationSends.delete(paneId);
    };
  }, [dictate, paneId]);

  const open = useCallback((): VoiceController => {
    // A Voice screen opened before the pane was known to be a chat made a
    // terminal controller (it types into the pane and registers with the
    // host). A chat needs its own.
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
      // Set from voice mode and the preference below; a dictation plays nothing.
      autoPlay: false,
      // Voice mode sends what was said; a dictation goes with the reply field's draft and files.
      submit: async (text) => {
        if (chatStore.getState().voiceMode[paneId] === true) return chatClient.send(paneId, text);
        const send = dictationSends.get(paneId);
        if (send) return send(text);
        // The chat was left before its dictation came back: it waits in the
        // chat's reply field, after what was typed there, rather than going
        // without it.
        writeDraft(serverIdentity, paneId, withDictation(readDraft(serverIdentity, paneId), text));
        toast("Your dictation is in the chat's reply field.");
      },
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
    if ((!controller || controller.isDisposed) && connected) setController(open());
  }, [connected, controller, open]);
  const playbackRate = useStore(prefsStore, (s) => s.voicePlaybackRate);
  const autoPlay = useStore(prefsStore, (s) => s.voiceAutoPlay);
  const tipDone = useStore(prefsStore, (s) => s.chatMicTipDone);

  const sessionKey = controller?.sessionKey ?? "";
  const phase = useVoice((s) => s.sessions[sessionKey]?.phase ?? "idle");
  const reply = useVoice((s) => latestReply(s.sessions[sessionKey]));
  const readiness = useVoice((s) => s.hostStatus.readiness);
  const recorderError = useVoice((s) => s.recorderError);
  const [pressing, setPressing] = useState(false);

  useEffect(() => controller?.setPlaybackRate(playbackRate), [controller, playbackRate]);
  // Before the focus below, which plays a reply that waits when autoplay is on.
  useEffect(() => controller?.setAutoPlay(voiceMode && autoPlay), [autoPlay, controller, voiceMode]);

  // Outside voice mode the host is asked whether voice is set up, without
  // warming it, so a hold can say why it records nothing.
  useEffect(() => {
    if (controller && !controller.isDisposed && connected && readiness === "unknown") void controller.refreshStatus(false, true);
  }, [connected, controller, readiness]);

  const active = voiceMode || pressing || phase !== "idle";
  useFocusEffect(useCallback(() => {
    if (!controller || controller.isDisposed || !active) return undefined;
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
      // A press the screen lost without its release (navigation) is over.
      setPressing(false);
    };
  }, [active, controller]));

  // A turn that ends in voice mode is read aloud: its newest reply, once.
  const previousTurn = useRef(turnOf(pane));
  const lastSpoken = useRef<string | undefined>(undefined);
  useEffect(() => {
    const before = previousTurn.current;
    previousTurn.current = turnOf(pane);
    if (!voiceMode || !controller || controller.isDisposed) return;
    const spoken = replyToSpeak(before, pane, lastSpoken.current);
    if (!spoken) return;
    lastSpoken.current = spoken.id;
    void controller.speakReply(spoken.text, Number(spoken.atUnixMillis) || Date.now());
  }, [controller, pane, voiceMode]);

  if (!controller) return undefined;
  const gone = pane?.status === "gone";
  const hint = (connected && !gone ? chatHint(pane) : "") || voiceHint({ connected, gone, readiness, recorderError });
  const live = controller;
  return {
    status: <VoiceStatusCard connected={connected} controller={controller} />,
    reply: reply ? <ChatReplyRow controller={controller} message={reply} /> : null,
    pane: <ChatVoicePane controller={controller} hint={hint} onKeyboard={onKeyboard} onStop={onStop} phase={phase} stopping={stopping} working={turnOf(pane) === ChatTurnState.WORKING} />,
    ready: readiness === "ready",
    dictation: {
      phase,
      refusal: hint,
      tip: !tipDone && hint === "",
      onPressIn: () => setPressing(true),
      onRelease: () => setPressing(false),
      onBegin: () => {
        prefsStore.getState().setChatMicTipDone();
        live.beginUtterance();
      },
      onSubmit: () => void live.endUtterance(),
      onLock: () => live.lockUtterance(),
      onCancel: () => void live.cancelUtterance(),
      onRefused: (reason) => {
        toast(reason);
        // A check that failed quietly on opening is tried again.
        if (readiness === "unknown" && connected) void live.refreshStatus(false, true);
      },
    },
  };
}

/** Why a chat that is still on screen takes no message now; empty when it does. */
function chatHint(pane: ChatPane | undefined): string {
  if (pane?.state?.connection === ChatConnection.ENDED || pane?.state?.connection === ChatConnection.FAILED) return "The agent has stopped";
  if (!pane || pane.status !== "open" || pane.resyncing) return "Waiting for the chat…";
  return "";
}

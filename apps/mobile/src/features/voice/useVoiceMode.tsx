// Voice in an agent's screen, the chat's or the terminal's
// (mobile-voice-mock.html). The reply field's mic does two things: a hold
// dictates one message, sent as Send would send it with what is typed and
// attached; a tap turns voice mode on, where the talk pane takes the reply
// field's place and each finished reply is read aloud. One `VoiceController`
// per pane does the recording, transcription and playback for both.
//
// A chat reads its own replies aloud with VOICE_SPEAK and registers nothing
// with the host. A terminal's agent has its replies pushed by the host, which
// it registers for while in voice mode.
//
// The microphone is only made ready while it can be used: in voice mode, or
// from the moment a finger is on the mic until its take is sent. Readying it
// takes the audio session (other apps duck, a headset moves to its call
// profile) and asks for the permission the first time, which opening a screen
// must not do.

import { useFocusEffect, useIsFocused } from "expo-router";
import { useKeepAwake } from "expo-keep-awake";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { AppState, Platform } from "react-native";
import { useStore } from "zustand";

import { androidConnectionServices } from "../../session/AndroidConnectionServices";
import { backgroundTimer } from "../../session/backgroundTimer";
import { getConnection, toast } from "../../session/connectionManager";
import { log } from "../../session/log";
import { muxflowSsh } from "../../ssh/MuxflowSsh";
import { prefsStore } from "../../store/prefsStore";
import { ChatReplyRow, ChatVoicePane } from "../chat/ui/ChatVoiceParts";
import type { ComposerDictation } from "../chat/ui/ComposerMic";
import { sharedAudio } from "./sharedAudio";
import type { VoiceController } from "./VoiceController";
import { useVoice } from "./voiceHooks";
import { voiceHint } from "./voiceHint";
import { voiceRegistry } from "./voiceRegistry";
import { VoiceStatusCard } from "./VoiceStatusCard";
import { latestReply } from "./voiceStore";

export interface VoiceModeParts {
  /** Voice mode: the setup card, until voice is ready on the host. */
  status: ReactNode;
  /** Voice mode: the newest reply's player. */
  reply: ReactNode;
  /** Voice mode: the talk pane, in the reply field's place once `ready`. */
  pane: ReactNode;
  /** Voice is set up on the host. */
  ready: boolean;
  /** The reply field's hold to dictate. */
  dictation: ComposerDictation;
}

/**
 * Types a transcript into the pane and submits it, as a terminal agent is
 * talked to. False when the agent left before the Enter, which is then not sent.
 */
export type TypeIntoPane = (text: string) => Promise<boolean>;

/**
 * Where a transcript goes. These outlive the screen, as the registry's
 * controller does, so they are the pane's, not the screen's.
 */
export interface VoiceRouting {
  /** A chat's controller: it reads its own replies aloud and registers nothing with the host. */
  chat: boolean;
  /** Whether the pane is in voice mode now, with or without its screen. */
  inVoiceMode: () => boolean;
  /** Voice mode's transcript. */
  send: (text: string, typeIntoPane: TypeIntoPane) => Promise<boolean | void>;
  /** A dictation that came back after its screen was left. */
  orphan: (text: string, typeIntoPane: TypeIntoPane) => Promise<boolean | void>;
  /** Rechecked after transcription and before Enter. */
  canSubmit: (paneId: string) => boolean;
}

/**
 * Where a dictated transcript goes, per pane: the screen's send, with the
 * draft and the files. Kept here rather than in the controller's options
 * because the registry's controller outlives the screen that made it.
 */
const dictationSends = new Map<string, (text: string, typeIntoPane: TypeIntoPane) => Promise<boolean | void>>();

export function useVoiceMode({ paneId, sessionId, serverIdentity, voiceMode, connected, gone, screenHint, working, stopping, onStop, onKeyboard, dictate, routing }: {
  paneId: string;
  sessionId: string;
  serverIdentity: string;
  voiceMode: boolean;
  connected: boolean;
  /** The agent has left: nothing can be said to it. */
  gone: boolean;
  /** Why the screen takes no message now, when it is not one `voiceHint` knows; empty when it does. */
  screenHint: string;
  working: boolean;
  /** Stop was pressed and the turn has not stopped yet. */
  stopping: boolean;
  onStop: () => void;
  /** Leaves voice mode for the reply field. */
  onKeyboard: () => void;
  /** Sends a dictated transcript as Send would. */
  dictate: (text: string, typeIntoPane: TypeIntoPane) => Promise<boolean | void>;
  routing: VoiceRouting;
}): { parts: VoiceModeParts | undefined; controller: VoiceController | undefined } {
  useEffect(() => {
    dictationSends.set(paneId, dictate);
    return () => {
      if (dictationSends.get(paneId) === dictate) dictationSends.delete(paneId);
    };
  }, [dictate, paneId]);

  const { chat, inVoiceMode, send, orphan, canSubmit } = routing;
  const open = useCallback((): VoiceController => {
    // A pane that changed between chat and terminal keeps no controller of the other kind.
    const existing = voiceRegistry.get(paneId);
    if (existing && existing.forChat !== chat) void voiceRegistry.end(paneId);
    return voiceRegistry.open({
      serverIdentity,
      paneId,
      sessionId,
      getConnection,
      ...sharedAudio(),
      appInForeground: () => AppState.currentState === "active",
      canPlayScreenOff: () => Platform.OS === "android" && prefsStore.getState().voicePlayScreenOff && androidConnectionServices().isScreenOffOrLocked(),
      backgroundTimer: backgroundTimer(),
      canSubmit,
      keepAlive: () => muxflowSsh().keepAlive(),
      playbackRate: prefsStore.getState().voicePlaybackRate,
      // Set from voice mode and the preference below; a dictation plays nothing.
      autoPlay: false,
      submit: async (text, typeIntoPane) => {
        if (inVoiceMode()) return send(text, typeIntoPane);
        const screen = dictationSends.get(paneId);
        return screen ? screen(text, typeIntoPane) : orphan(text, typeIntoPane);
      },
      registerWithHost: !chat,
      toast,
      log,
    });
  }, [canSubmit, chat, inVoiceMode, orphan, paneId, send, serverIdentity, sessionId]);
  // The registry keeps one controller per pane. It outlives a reconnect, so
  // a brief drop does not cancel a take or stop a reply; one disposed
  // meanwhile (host forgotten, server replaced) is replaced once connected.
  const [controller, setController] = useState<VoiceController | undefined>(undefined);
  useEffect(() => {
    if ((!controller || controller.isDisposed) && connected) setController(open());
  }, [connected, controller, open]);
  const playbackRate = useStore(prefsStore, (s) => s.voicePlaybackRate);
  const autoPlay = useStore(prefsStore, (s) => s.voiceAutoPlay);
  const tipDone = useStore(prefsStore, (s) => s.chatMicTipDone);
  const keepAwake = useStore(prefsStore, (s) => s.voiceKeepAwake);

  const sessionKey = controller?.sessionKey ?? "";
  const phase = useVoice((s) => s.sessions[sessionKey]?.phase ?? "idle");
  const reply = useVoice((s) => latestReply(s.sessions[sessionKey]));
  const readiness = useVoice((s) => s.hostStatus.readiness);
  const recorderError = useVoice((s) => s.recorderError);
  const [pressing, setPressing] = useState(false);
  // The screen stays on only while this screen is the one in front.
  const focused = useIsFocused();

  useEffect(() => controller?.setPlaybackRate(playbackRate), [controller, playbackRate]);
  // Before the focus below, which plays a reply that waits when autoplay is on.
  useEffect(() => controller?.setAutoPlay(voiceMode && autoPlay), [autoPlay, controller, voiceMode]);

  // Outside voice mode the host is asked whether voice is set up, without
  // warming it, so a hold can say why it records nothing.
  useEffect(() => {
    if (controller && !controller.isDisposed && connected && readiness === "unknown") void controller.refreshStatus(false, true);
  }, [connected, controller, readiness]);

  // A terminal's replies are pushed by the host only in voice mode: a
  // dictation registers nothing, and leaving voice mode or the screen ends it.
  useEffect(() => {
    if (!voiceMode || !controller || controller.isDisposed) return undefined;
    const live = controller;
    return () => void voiceRegistry.release(live);
  }, [controller, voiceMode]);

  const active = voiceMode || pressing || phase !== "idle";
  useFocusEffect(useCallback(() => {
    if (!controller || controller.isDisposed || !active) return undefined;
    const live = controller;
    live.focus(voiceMode);
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
  }, [active, controller, voiceMode]));

  if (!controller) return { parts: undefined, controller };
  const hint = (connected && !gone ? screenHint : "") || voiceHint({ connected, gone, readiness, recorderError });
  const live = controller;
  return {
    controller,
    parts: {
      status: <VoiceStatusCard connected={connected} controller={controller} />,
      reply: reply ? <ChatReplyRow controller={controller} message={reply} /> : null,
      pane: (
        <>
          {keepAwake && focused ? <VoiceWakeLock /> : null}
          <ChatVoicePane controller={controller} hint={hint} onKeyboard={onKeyboard} onStop={onStop} onTipDone={() => prefsStore.getState().setChatMicTipDone()} phase={phase} stopping={stopping} tip={!tipDone} working={working} />
        </>
      ),
      ready: readiness === "ready",
      dictation: {
        phase,
        refusal: hint,
        onPressIn: () => setPressing(true),
        onRelease: () => setPressing(false),
        onBegin: () => {
          prefsStore.getState().setChatMicTipDone();
          live.beginUtterance();
        },
        onSubmit: () => void live.endUtterance(),
        onLock: () => live.lockUtterance(),
        onCancel: () => void live.cancelUtterance(),
        onRefused: (refused) => {
          toast(refused);
          // A check that failed quietly on opening is tried again.
          if (readiness === "unknown" && connected) void live.refreshStatus(false, true);
        },
      },
    },
  };
}

/** Keeps the screen on while the talk pane shows, when the preference asks for it. */
function VoiceWakeLock() {
  useKeepAwake("muxflow-voice-mode", { suppressDeactivateWarnings: true });
  return null;
}

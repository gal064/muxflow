// Voice in a chat: the shared voice mode (../voice/useVoiceMode), with a
// chat's routing. Each finished reply is read aloud with VOICE_SPEAK, so the
// controller registers nothing with the host and no reply is pushed twice.

import { useEffect, useMemo, useRef } from "react";

import { ChatConnection, ChatTurnState } from "../../protocol/gen/chat_pb";
import { toast } from "../../session/connectionManager";
import { useVoiceMode, type TypeIntoPane, type VoiceModeParts, type VoiceRouting } from "../voice/useVoiceMode";
import { replyToSpeak, turnOf, withDictation, type ChatPane } from "./chatModel";
import { chatClient, chatStore } from "./chatRuntime";
import { readDraft, writeDraft } from "./drafts";

export type ChatVoiceParts = VoiceModeParts;

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
  const routing = useMemo((): VoiceRouting => ({
    chat: true,
    inVoiceMode: () => chatStore.getState().voiceMode[paneId] === true,
    // Voice mode sends what was said; a dictation goes with the reply field's draft and files.
    send: (text) => chatClient.send(paneId, text),
    // The chat was left before its dictation came back: it waits in the
    // chat's reply field, after what was typed there, rather than going
    // without it.
    orphan: async (text) => {
      writeDraft(serverIdentity, paneId, withDictation(readDraft(serverIdentity, paneId), text));
      toast("Your dictation is in the chat's reply field.");
    },
    canSubmit: (target) => chatClient.pane(target)?.status === "open",
  }), [paneId, serverIdentity]);
  const dictateText = useMemo(() => (text: string, _typeIntoPane: TypeIntoPane) => dictate(text), [dictate]);
  const { parts, controller } = useVoiceMode({
    paneId,
    sessionId,
    serverIdentity,
    voiceMode,
    connected,
    gone: pane?.status === "gone",
    screenHint: chatHint(pane),
    working: turnOf(pane) === ChatTurnState.WORKING,
    stopping,
    onStop,
    onKeyboard,
    dictate: dictateText,
    routing,
  });

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

  return parts;
}

/** Why a chat that is still on screen takes no message now; empty when it does. */
function chatHint(pane: ChatPane | undefined): string {
  if (pane?.state?.connection === ChatConnection.ENDED || pane?.state?.connection === ChatConnection.FAILED) return "The agent has stopped";
  if (!pane || pane.status !== "open" || pane.resyncing) return "Waiting for the chat…";
  return "";
}

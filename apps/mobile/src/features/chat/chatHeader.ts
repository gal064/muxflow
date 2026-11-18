// What the chat header's second line says (mobile-chat-mock.html, S8): the
// chat's state, then the model and effort it runs with.

import { ChatConnection, ChatTurnState } from "../../protocol/gen/chat_pb";
import { effortLabel, modelLabel, turnOf, type ChatPane } from "./chatModel";

/** The dot's colour: the working ink, the one that asks for you, or the faint one. */
export type HeaderTone = "working" | "waiting" | "quiet";

export interface HeaderState {
  word: string;
  tone: HeaderTone;
}

export interface HeaderLine {
  /** Absent while the banner under the header says what is going on. */
  state?: HeaderState;
  /** The state, model and effort that are known, joined with " · ". */
  text: string;
}

/**
 * The chat's state in a word. None while the phone is not connected or the
 * chat is still opening (the banner says that), nor for a chat the host
 * refused to open or an agent that is still connecting.
 */
export function headerState(pane: ChatPane | undefined, connected: boolean): HeaderState | undefined {
  if (!connected || !pane || pane.status === "opening" || pane.status === "error") return undefined;
  const connection = pane.state?.connection;
  if (pane.status === "gone" || connection === ChatConnection.ENDED || connection === ChatConnection.FAILED) return { word: "Ended", tone: "quiet" };
  switch (turnOf(pane)) {
    case ChatTurnState.WORKING:
      return { word: "Working", tone: "working" };
    case ChatTurnState.WAITING:
      return { word: "Waiting for you", tone: "waiting" };
    case ChatTurnState.IDLE:
      return connection === ChatConnection.READY ? { word: "Idle", tone: "quiet" } : undefined;
    default:
      return undefined;
  }
}

/** The second line, or undefined with nothing to say. */
export function headerLine(pane: ChatPane | undefined, connected: boolean): HeaderLine | undefined {
  const state = headerState(pane, connected);
  const parts = [state?.word ?? "", modelLabel(pane), effortLabel(pane?.state?.options?.effort ?? "")].filter((part) => part !== "");
  if (parts.length === 0) return undefined;
  const text = parts.join(" · ");
  return state ? { state, text } : { text };
}

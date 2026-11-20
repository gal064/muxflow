// What the chat header's second line says (mobile-chat-mock.html, S8): the
// chat's state, then the model and effort it runs with.

import { ChatConnection, ChatTurnState } from "../../protocol/gen/chat_pb";
import type { AgentDisplayState } from "../../store/selectors";
import { effortLabel, modelLabel, turnOf, type ChatPane } from "./chatModel";

export interface HeaderState {
  word: string;
  /** What the agent's mark draws for it, as the agent list does. */
  mark: AgentDisplayState;
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
export function headerState(pane: ChatPane | undefined, connected: boolean, finished = false): HeaderState | undefined {
  if (!connected || !pane || pane.status === "opening" || pane.status === "error") return undefined;
  const connection = pane.state?.connection;
  if (pane.status === "gone" || connection === ChatConnection.ENDED || connection === ChatConnection.FAILED) return { word: "Ended", mark: "idle" };
  switch (turnOf(pane)) {
    case ChatTurnState.WORKING:
      return { word: "Working", mark: "working" };
    case ChatTurnState.WAITING:
      return { word: "Waiting for you", mark: "blocked" };
    case ChatTurnState.IDLE:
      if (connection !== ChatConnection.READY) return undefined;
      return finished ? { word: "Finished", mark: "done" } : { word: "Idle", mark: "idle" };
    default:
      return undefined;
  }
}

/**
 * The second line, or undefined with nothing to say. `finished` is the
 * agent's unseen completion, which the agent list marks too: an idle chat
 * says Finished until it is seen.
 */
export function headerLine(pane: ChatPane | undefined, connected: boolean, finished = false): HeaderLine | undefined {
  const state = headerState(pane, connected, finished);
  const parts = [state?.word ?? "", modelLabel(pane), effortLabel(pane?.state?.options?.effort ?? "")].filter((part) => part !== "");
  if (parts.length === 0) return undefined;
  const text = parts.join(" · ");
  return state ? { state, text } : { text };
}

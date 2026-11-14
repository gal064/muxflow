// The app's one chat client. `connectionManager` feeds it events and
// connection changes and tells it where the connection is.

import { log } from "../../session/log";
import { sessionStore } from "../../store/sessionStore";
import { ChatClient, createChatStore, type ChatConnection } from "./ChatClient";
import { refusalText } from "./refusals";

let source: () => ChatConnection | null = () => null;
let toast: (message: string) => void = () => undefined;

/** Called once by `connectionManager`, which owns the connection and the toasts. */
export function bindChat(next: () => ChatConnection | null, showToast: (message: string) => void): void {
  source = next;
  toast = showToast;
}

export const chatStore = createChatStore();

export const chatClient = new ChatClient({
  store: chatStore,
  getConnection: () => source(),
  serverIdentity: () => sessionStore.getState().serverIdentity,
  onUnconfirmed: (_paneId, text, error) => {
    const quoted = `"${text.length > 40 ? `${text.slice(0, 40)}…` : text}"`;
    toast(error === undefined ? `Couldn't confirm ${quoted} was sent.` : `Not sent: ${quoted}. ${refusalText(error)}`);
  },
  log,
});

export function setVoiceMode(paneId: string, on: boolean): void {
  chatStore.setState((state) => ({ ...state, voiceMode: { ...state.voiceMode, [paneId]: on } }));
}

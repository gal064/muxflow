// What is typed in a chat's reply field and not sent yet, kept per chat for as
// long as the app runs (mobile-chat-mock.html, S5): leaving a chat and coming
// back shows it again. A chat is a pane of one tmux server, so the same pane
// id on another server is another chat. Nothing is written to disk.

const drafts = new Map<string, string>();

function key(serverIdentity: string, paneId: string): string {
  return JSON.stringify([serverIdentity, paneId]);
}

/** The draft kept for the chat, or "". Before the server is known there is none. */
export function readDraft(serverIdentity: string, paneId: string): string {
  if (!serverIdentity) return "";
  return drafts.get(key(serverIdentity, paneId)) ?? "";
}

/** Keeps `text` as the chat's draft. Nothing but spaces is no draft, and takes the kept one away. */
export function writeDraft(serverIdentity: string, paneId: string, text: string): void {
  if (!serverIdentity) return;
  if (text.trim() === "") drafts.delete(key(serverIdentity, paneId));
  else drafts.set(key(serverIdentity, paneId), text);
}

/** Forgets every draft. For the tests. */
export function forgetDrafts(): void {
  drafts.clear();
}

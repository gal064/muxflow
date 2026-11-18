import { useCallback, useEffect, useRef, useState, type SetStateAction } from "react";

import type { ChatItem } from "../../protocol/gen/chat_pb";
import { readDraft, writeDraft } from "./drafts";

export interface ReplyDraft {
  /** What the reply field holds. */
  draft: string;
  setDraft: (text: SetStateAction<string>) => void;
  /** Your message being edited, when one is. */
  editing: ChatItem | undefined;
  /** Ends the edit (`undefined`) and leaves the field as it is: it was sent, or its message is gone. */
  setEditing: (item: ChatItem | undefined) => void;
  /** Starts editing `item`: the field takes its text. */
  edit: (item: ChatItem) => void;
  /** Leaves the edit, and puts back what the field held before it. */
  cancelEdit: () => void;
  /** The edit was sent: the field goes back to the draft from before it. */
  sentEdit: () => void;
}

/**
 * The reply field of one chat screen (mobile-chat-mock.html, S5). What is
 * typed is the chat's draft: it is kept while the app runs (`drafts.ts`), so
 * the screen shows it again when it is opened next. Editing an earlier
 * message borrows the field: the draft from before stays the kept one, and
 * comes back when the edit is cancelled.
 *
 * `paneId` is fixed for a screen (the route keys the screen by it).
 */
export function useReplyDraft(serverIdentity: string, paneId: string): ReplyDraft {
  const [draft, setDraftState] = useState(() => readDraft(serverIdentity, paneId));
  const [editing, setEditingState] = useState<ChatItem | undefined>(undefined);
  const now = useRef({ draft, editing });
  now.current = { draft, editing };
  // What the field held when the edit began.
  const before = useRef("");
  // The field still holds the text of an edit that ended without it being
  // sent or cancelled. That is not a draft until it is changed.
  const leftover = useRef(false);

  // The tmux server whose chat this screen shows. A screen opened before the
  // phone has connected learns it later; from then on it stays, because a
  // server that replaces it has other chats behind the same pane ids.
  const server = useRef(serverIdentity);
  useEffect(() => {
    if (server.current || !serverIdentity) return;
    server.current = serverIdentity;
    const kept = readDraft(serverIdentity, paneId);
    if (!kept) return;
    // What was typed meanwhile stays; an empty field takes the chat's draft.
    if (now.current.editing) before.current ||= kept;
    else setDraftState((current) => current || kept);
  }, [paneId, serverIdentity]);

  // An edit in progress is not a draft.
  useEffect(() => {
    if (!editing && !leftover.current) writeDraft(server.current, paneId, draft);
  }, [draft, editing, paneId, serverIdentity]);

  const setDraft = useCallback((text: SetStateAction<string>) => {
    leftover.current = false;
    setDraftState(text);
  }, []);

  const setEditing = useCallback((item: ChatItem | undefined) => {
    if (!item && now.current.editing) leftover.current = true;
    setEditingState(item);
  }, []);

  const edit = useCallback((item: ChatItem) => {
    // Going from one edit to another keeps the draft from before the first.
    if (!now.current.editing) before.current = now.current.draft;
    leftover.current = false;
    setEditingState(item);
    setDraftState(item.text);
  }, []);

  const cancelEdit = useCallback(() => {
    if (!now.current.editing) return;
    leftover.current = false;
    setEditingState(undefined);
    setDraftState(before.current);
    before.current = "";
  }, []);

  // The edited message may already have left the timeline, which ended the
  // edit; the field still holds the sent text either way.
  const sentEdit = useCallback(() => {
    leftover.current = false;
    setEditingState(undefined);
    setDraftState(before.current);
    before.current = "";
  }, []);

  return { draft, setDraft, editing, setEditing, edit, cancelEdit, sentEdit };
}

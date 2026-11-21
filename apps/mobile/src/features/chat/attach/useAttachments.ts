// One chat screen's files (docs/design/mobile-chat-plan.md §2 "M6"): the
// queue behind the reply field's chips, the pickers that fill it, and what a
// send takes from it. The screen owns it; leaving the chat stops its uploads.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "zustand";

import type { OutgoingAttachment } from "../chatModel";
import { AttachmentQueue, MAX_ATTACHMENTS, TOO_MANY_ATTACHMENTS, type AttachmentEntry, type AttachmentSource, type HostedFile, type Uploader } from "./attachmentQueue";
import { isImage } from "./attachState";
import { localImageFor, rememberLocalImage } from "./localImages";
import type { AttachChoice, PickedFile } from "./pickers";

/** What the files need from the app. `attachRuntime` has the real ones; a test passes its own. */
export interface AttachDeps {
  upload: Uploader;
  /** Opens a picker; answers nothing when the user backed out, and rejects with a sentence to show. */
  pick: (choice: AttachChoice, limit: number) => Promise<PickedFile[]>;
  /** The chosen file as bytes to upload. Throws a sentence to show when it cannot be read. */
  toSource: (file: PickedFile) => AttachmentSource;
  /** Says a refusal that has no chip to sit on. */
  say: (message: string) => void;
}

/** The files a message takes with it. */
export interface TakenFiles {
  files: OutgoingAttachment[];
  /** The message was accepted: its files leave the reply field for good. */
  sent: () => void;
  /** The message was refused: its files are on the reply field again. */
  back: () => void;
}

export interface ChatAttachments {
  /** The chips on the reply field, in the order the files were added. */
  entries: readonly AttachmentEntry[];
  /** A choice from the attach sheet: opens its picker and queues what was chosen. */
  choose: (choice: AttachChoice) => void;
  /** Takes a file off the reply field, stopping its upload. */
  remove: (id: string) => void;
  /**
   * The files for the message being sent, which leave the reply field while
   * it is on its way. Call it only when every file is on the host.
   */
  take: () => TakenFiles;
  /** Replaces the files with ones the host already holds: those of a message being edited. */
  replace: (files: readonly HostedFile[]) => void;
  /** Empties the reply field of files, stopping an upload that runs. */
  clear: () => void;
}

const NOTHING: ReadonlySet<string> = new Set();

/**
 * Adds what a picker answered, each file uploading in its turn. Answers the
 * sentence to show when any was left out.
 */
export function addPicked(queue: AttachmentQueue, files: readonly PickedFile[], toSource: (file: PickedFile) => AttachmentSource): string | undefined {
  let refusal: string | undefined;
  for (const file of files) {
    try {
      const result = queue.add(toSource(file));
      if ("refused" in result) refusal ??= result.refused;
    } catch (error) {
      refusal ??= error instanceof Error && error.message !== "" ? error.message : "That file could not be read.";
    }
  }
  return refusal;
}

/** The entries that are on the host, as a message carries them. */
export function outgoingFiles(entries: readonly AttachmentEntry[]): OutgoingAttachment[] {
  return entries.flatMap((entry) => (entry.status === "ready" && entry.hostPath !== undefined
    ? [{
      path: entry.hostPath,
      name: entry.name,
      mediaType: entry.mediaType,
      sizeBytes: entry.sizeBytes,
      ...(entry.localUri === undefined ? {} : { localUri: entry.localUri }),
    }]
    : []));
}

/**
 * Sends a message with the reply field's files. They leave the field while
 * it is on its way, for good once the host took it, and are back on it when
 * the host refused, which is passed on.
 */
export async function sendWithFiles(files: Pick<ChatAttachments, "take">, send: (attachments: OutgoingAttachment[]) => Promise<void>): Promise<void> {
  const taken = files.take();
  try {
    await send(taken.files);
  } catch (error) {
    taken.back();
    throw error;
  }
  taken.sent();
}

export function useAttachments(paneId: string, deps: AttachDeps): ChatAttachments {
  const { upload, pick, toSource, say } = deps;
  // Another chat is another reply field, so `paneId` makes another queue.
  const queue = useMemo(() => new AttachmentQueue(upload), [paneId, upload]);
  const live = useRef<AttachmentQueue | undefined>(undefined);
  useEffect(() => {
    live.current = queue;
    return () => {
      live.current = undefined;
      // Leaving the chat: an upload still running is stopped and its partial removed.
      queue.clear();
    };
  }, [queue]);

  const held = useStore(queue.store, (state) => state.entries);
  // The files of a message on its way: still in the queue until the host takes it, off the reply field meanwhile.
  const [away, setAway] = useState<ReadonlySet<string>>(NOTHING);
  const entries = useMemo(() => (away.size === 0 ? held : held.filter((entry) => !away.has(entry.id))), [away, held]);

  const choose = useCallback((choice: AttachChoice) => {
    const room = MAX_ATTACHMENTS - queue.entries.length;
    if (room <= 0) {
      say(TOO_MANY_ATTACHMENTS);
      return;
    }
    pick(choice, room).then(
      (files) => {
        // The chat was left while the picker was open: nothing is uploaded for it.
        if (live.current !== queue) return;
        const refusal = addPicked(queue, files, toSource);
        if (refusal !== undefined) say(refusal);
      },
      (error: unknown) => say(error instanceof Error && error.message !== "" ? error.message : "That could not be attached."),
    );
  }, [pick, queue, say, toSource]);

  const remove = useCallback((id: string) => queue.remove(id), [queue]);

  const take = useCallback((): TakenFiles => {
    const going = queue.entries.filter((entry) => entry.status === "ready" && entry.hostPath !== undefined);
    const ids = new Set(going.map((entry) => entry.id));
    const files = outgoingFiles(going);
    // Before the message goes: the host may show it back before it answers,
    // and its chip looks here for the thumbnail.
    for (const file of files) {
      if (file.localUri !== undefined && isImage(file)) rememberLocalImage(file.path, file.localUri, file.sizeBytes);
    }
    if (ids.size > 0) setAway(ids);
    return {
      files,
      sent: () => {
        for (const id of ids) queue.remove(id);
        setAway(NOTHING);
      },
      back: () => setAway(NOTHING),
    };
  }, [queue]);

  const replace = useCallback((files: readonly HostedFile[]) => {
    queue.clear();
    setAway(NOTHING);
    for (const file of files) {
      const localUri = file.localUri ?? localImageFor(file.path, file.sizeBytes);
      queue.addHosted(localUri === undefined ? file : { ...file, localUri });
    }
  }, [queue]);

  const clear = useCallback(() => {
    queue.clear();
    setAway(NOTHING);
  }, [queue]);

  return useMemo(() => ({ entries, choose, remove, take, replace, clear }), [choose, clear, entries, remove, replace, take]);
}

// The files waiting on one chat's reply field (docs/design/mobile-chat-plan.md
// §2 "M6"). A file is added, uploads in its turn, and is ready once it is on
// the host; Send waits until every one is. The store holds plain data for the
// composer's chips to draw. The byte sources and the running upload stay in
// here.

import { createStore, type StoreApi } from "zustand/vanilla";
import type { ChatSendAttachment } from "../../../protocol/requests";
import { mediaTypeForName } from "./mediaType";
import { UploadError, uploadRefusal, type ByteSource, type UploadedFile } from "./upload";

/** The most files one message carries; the host holds the same bound (`MAX_ATTACHMENTS`). */
export const MAX_ATTACHMENTS = 10;
/** The host's own sentence for one more than that. */
export const TOO_MANY_ATTACHMENTS = `A message takes at most ${MAX_ATTACHMENTS} attachments.`;

export type AttachmentStatus = "queued" | "uploading" | "ready" | "failed";

export interface AttachmentEntry {
  /** Unique for the life of the queue, across `clear()`. */
  id: string;
  /** The file's own name, which is what the message shows. */
  name: string;
  sizeBytes: number;
  /** An image type such as "image/png", from the name's extension; "" for any other file. */
  mediaType: string;
  /** The file on the phone, for a thumbnail. */
  localUri?: string;
  status: AttachmentStatus;
  /** Bytes the host has acknowledged. */
  sentBytes: number;
  /** Why it failed, as a sentence to show on its chip. */
  error?: string;
  /** Where it is on the host, once it is ready. */
  hostPath?: string;
}

export interface AttachmentQueueState {
  /** In the order they were added. */
  entries: AttachmentEntry[];
}

export type AttachmentStore = StoreApi<AttachmentQueueState>;

export function createAttachmentStore(): AttachmentStore {
  return createStore<AttachmentQueueState>(() => ({ entries: [] }));
}

/** A file to attach: its bytes, and where it is on the phone when it has a place there. */
export interface AttachmentSource extends ByteSource {
  localUri?: string;
}

/** A file the host already holds: `ChatAttachment` as an item carries it, with its place on the phone when that is known. */
export interface HostedFile {
  path: string;
  name: string;
  mediaType: string;
  sizeBytes: number;
  localUri?: string;
}

/** One upload. The app binds `uploadFile` to its bulk connection; rejects with a sentence to show. */
export type Uploader = (
  source: ByteSource,
  signal: AbortSignal,
  onProgress: (sentBytes: number, totalBytes: number) => void,
) => Promise<UploadedFile>;

/** `refused` is a sentence to show; nothing was added. */
export type AddResult = { entry: AttachmentEntry } | { refused: string };

export class AttachmentQueue {
  readonly store: AttachmentStore;
  private readonly sources = new Map<string, AttachmentSource>();
  /** The one upload that runs, and how to stop it. */
  private running: { id: string; abort: AbortController } | undefined;
  private nextId = 1;

  constructor(private readonly upload: Uploader, store: AttachmentStore = createAttachmentStore()) {
    this.store = store;
  }

  get entries(): readonly AttachmentEntry[] {
    return this.store.getState().entries;
  }

  /**
   * Adds a file and uploads it in its turn. A file the phone will not send
   * (too large) is added as failed, so its chip says why.
   */
  add(source: AttachmentSource): AddResult {
    if (this.entries.length >= MAX_ATTACHMENTS) return { refused: TOO_MANY_ATTACHMENTS };
    const refusal = uploadRefusal(source.name, source.size);
    const entry: AttachmentEntry = {
      id: `attachment-${this.nextId++}`,
      name: source.name,
      sizeBytes: source.size,
      mediaType: mediaTypeForName(source.name),
      ...(source.localUri === undefined ? {} : { localUri: source.localUri }),
      status: refusal === undefined ? "queued" : "failed",
      sentBytes: 0,
      ...(refusal === undefined ? {} : { error: refusal }),
    };
    if (refusal === undefined) this.sources.set(entry.id, source);
    this.store.setState((state) => ({ entries: [...state.entries, entry] }));
    this.pump();
    return { entry };
  }

  /**
   * Adds a file that is already on the host, as the files of a message being
   * edited are: nothing uploads, and it goes with the next send as it is.
   */
  addHosted(file: HostedFile): AddResult {
    if (this.entries.length >= MAX_ATTACHMENTS) return { refused: TOO_MANY_ATTACHMENTS };
    const entry: AttachmentEntry = {
      id: `attachment-${this.nextId++}`,
      name: file.name,
      sizeBytes: file.sizeBytes,
      mediaType: file.mediaType || mediaTypeForName(file.name),
      ...(file.localUri === undefined ? {} : { localUri: file.localUri }),
      status: "ready",
      sentBytes: file.sizeBytes,
      hostPath: file.path,
    };
    this.store.setState((state) => ({ entries: [...state.entries, entry] }));
    return { entry };
  }

  /**
   * Takes a file out. One that is uploading is stopped and its partial
   * removed; one already on the host is left there for the host's retention.
   */
  remove(id: string): void {
    if (!this.entries.some((entry) => entry.id === id)) return;
    this.sources.delete(id);
    this.store.setState((state) => ({ entries: state.entries.filter((entry) => entry.id !== id) }));
    // The upload ends on its own once aborted, and the next file goes then.
    if (this.running?.id === id) this.running.abort.abort();
  }

  /** There is something to send and all of it is on the host. */
  ready(): boolean {
    const entries = this.entries;
    return entries.length > 0 && entries.every((entry) => entry.status === "ready");
  }

  /** The files as SEND names them. Only those on the host: call it when `ready()`. */
  attachmentsForSend(): ChatSendAttachment[] {
    return this.entries.flatMap((entry) => (entry.status === "ready" && entry.hostPath !== undefined ? [{ path: entry.hostPath, name: entry.name }] : []));
  }

  /** Empties the queue after a send, stopping an upload that still runs. */
  clear(): void {
    this.sources.clear();
    this.store.setState({ entries: [] });
    this.running?.abort.abort();
  }

  /** Starts the next queued file, unless one runs. */
  private pump(): void {
    if (this.running) return;
    const next = this.entries.find((entry) => entry.status === "queued");
    if (!next) return;
    const source = this.sources.get(next.id);
    if (!source) return;
    const id = next.id;
    const abort = new AbortController();
    this.running = { id, abort };
    this.patch(id, { status: "uploading" });
    // Inside the chain, so an uploader that throws at once fails its file too.
    void Promise.resolve()
      .then(() => this.upload(source, abort.signal, (sentBytes) => this.patch(id, { sentBytes })))
      .then(
        (uploaded) => this.patch(id, { status: "ready", sentBytes: uploaded.sizeBytes, hostPath: uploaded.hostPath }),
        (error: unknown) => this.patch(id, { status: "failed", error: failureText(error) }),
      )
      .finally(() => {
        this.sources.delete(id);
        this.running = undefined;
        this.pump();
      });
  }

  /** Changes one entry. An entry that was removed meanwhile stays gone. */
  private patch(id: string, change: Partial<AttachmentEntry>): void {
    this.store.setState((state) => {
      if (!state.entries.some((entry) => entry.id === id)) return state;
      return { entries: state.entries.map((entry) => (entry.id === id ? { ...entry, ...change } : entry)) };
    });
  }
}

function failureText(error: unknown): string {
  if (error instanceof UploadError || (error instanceof Error && error.message !== "")) return error.message;
  return "The upload failed.";
}

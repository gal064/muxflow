// Follows the chats this phone has on screen (docs/design/chat-plan.md §4).
// A screen `acquire`s a pane; the first holder sends OPEN and the last one to
// let go sends CLOSE. OPEN is sent again on a lost change, after every
// reconnect (a RESYNC_REQUIRED reconnects the phone too), and when the host
// could not queue the snapshot. The store holds what the screens draw.

import { createStore, type StoreApi } from "zustand/vanilla";
import type { MessageInitShape } from "@bufbuild/protobuf";
import type { ChatAnswerSchema, ChatEvent, ChatOptionsSchema } from "../../protocol/gen/chat_pb";
import type { Request, Response } from "../../protocol/gen/envelope_pb";
import { HostError } from "../../protocol/HostConnection";
import {
  chatAnswer,
  chatClose,
  chatFork,
  chatInterrupt,
  chatLoadOlder,
  chatOpen,
  chatRewind,
  chatSend,
  chatSetOptions,
  chatSideFork,
  chatStopTask,
  type ChatTarget,
} from "../../protocol/requests";
import { applyChatEvent, applyOlderPage, newChatPane, newClientMessageId, type ChatPane, type OlderPage, type OutgoingAttachment, type OutgoingMessage } from "./chatModel";

export interface ChatStoreState {
  panes: Record<string, ChatPane>;
  /** Panes whose screen has voice mode on. Kept while the app runs. */
  voiceMode: Record<string, boolean>;
}

export type ChatStore = StoreApi<ChatStoreState>;

export function createChatStore(): ChatStore {
  return createStore<ChatStoreState>(() => ({ panes: {}, voiceMode: {} }));
}

/** What the client needs from a connection. `HostConnection` is one. */
export interface ChatConnection {
  readonly state: string;
  request(request: Request): Promise<Response>;
}

export interface ChatClientOptions {
  store: ChatStore;
  getConnection: () => ChatConnection | null;
  /** The tmux server the phone is following now. */
  serverIdentity: () => string;
  log?: (line: string) => void;
  /**
   * A message whose SEND answer was lost left the screen unconfirmed: refused
   * when sent again (`error`), or its chat closed before it could be.
   */
  onUnconfirmed?: (paneId: string, text: string, error?: unknown) => void;
  /** The pause before an OPEN refused with `chat_queue_full` is sent again. */
  retryDelayMs?: number;
  setTimer?: (delayMs: number, run: () => void) => unknown;
}

/** An OPEN the host could not queue is tried this many times before the screen says so. */
export const OPEN_ATTEMPTS = 5;

export class ChatClient {
  private readonly holders = new Map<string, number>();
  /** The OPEN on its way per pane; a newer one replaces it, so an older answer does not clear it. */
  private readonly opening = new Map<string, symbol>();
  /** Panes that asked for a fresh snapshot while an OPEN was on its way; it goes when that one is answered. */
  private readonly reopenWanted = new Set<string>();
  /** One token per hold of a pane, from its first holder to its last. */
  private readonly holds = new Map<string, symbol>();
  private readonly retries = new Map<string, number>();

  constructor(private readonly options: ChatClientOptions) {}

  get store(): ChatStore {
    return this.options.store;
  }

  pane(paneId: string): ChatPane | undefined {
    return this.options.store.getState().panes[paneId];
  }

  /** A screen shows `paneId`. Returns its release. */
  acquire(paneId: string): () => void {
    const count = this.holders.get(paneId) ?? 0;
    this.holders.set(paneId, count + 1);
    // A screen still showing a pane of a replaced tmux server holds the same
    // id; the new pane behind it is another chat, followed afresh.
    const held = this.pane(paneId);
    const server = this.options.serverIdentity();
    const stale = count > 0 && held?.status === "gone" && server !== "" && held.serverIdentity !== server;
    if (count === 0 || stale) {
      this.holds.set(paneId, Symbol(paneId));
      this.put(newChatPane(paneId, this.options.serverIdentity()));
      this.open(paneId);
    }
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.release(paneId);
    };
  }

  private release(paneId: string): void {
    const count = (this.holders.get(paneId) ?? 1) - 1;
    if (count > 0) {
      this.holders.set(paneId, count);
      return;
    }
    this.holders.delete(paneId);
    this.holds.delete(paneId);
    this.dropUnsure(paneId);
    this.retries.delete(paneId);
    this.reopenWanted.delete(paneId);
    // A screen that comes back before this OPEN is answered needs its own OPEN after the CLOSE.
    this.opening.delete(paneId);
    const pane = this.pane(paneId);
    this.remove(paneId);
    if (pane && pane.status !== "gone") this.fireAndForget(chatClose(this.target(pane)), "close", paneId);
  }

  /** Sends OPEN unless one is already on its way; the snapshot arrives as an event. */
  open(paneId: string): void {
    if (!this.holders.has(paneId)) return;
    if (this.opening.has(paneId)) {
      // Its snapshot may already be applied (it comes before the answer): ask again once answered.
      this.reopenWanted.add(paneId);
      return;
    }
    const pane = this.pane(paneId);
    if (!pane || pane.status === "gone") return;
    const connection = this.live();
    // Not connected: `onConnected` opens every held pane.
    if (!connection) return;
    const token = Symbol(paneId);
    this.opening.set(paneId, token);
    this.log(`chat.open pane=${paneId} reason=${pane.revision === undefined ? "first" : "again"}`);
    connection.request(chatOpen(this.target(pane))).then(
      () => {
        if (this.opening.get(paneId) !== token) return;
        this.opening.delete(paneId);
        this.retries.delete(paneId);
        if (this.reopenWanted.delete(paneId)) this.open(paneId);
      },
      (error: unknown) => {
        if (this.opening.get(paneId) !== token) return;
        this.opening.delete(paneId);
        this.reopenWanted.delete(paneId);
        this.onOpenRefused(paneId, error);
      },
    );
  }

  /** The screen's Retry after a refused OPEN. */
  retry(paneId: string): void {
    const pane = this.pane(paneId);
    if (!pane || pane.status !== "error") return;
    this.retries.delete(paneId);
    this.put({ ...pane, status: "opening", error: "" });
    this.open(paneId);
  }

  private onOpenRefused(paneId: string, error: unknown): void {
    if (!this.holders.has(paneId)) return;
    if (!(error instanceof HostError)) {
      this.log(`chat.open.failed pane=${paneId} ${error instanceof Error ? error.name : "error"}`);
      // The connection went: the reconnect opens it again. The host did not
      // answer on a live connection: the screen offers Retry.
      if (this.live()) this.update(paneId, (pane) => ({ ...pane, status: "error", resyncing: false, error: "The host didn't answer." }));
      return;
    }
    if (error.code === "chat_server_changed") {
      this.update(paneId, (pane) => ({ ...pane, status: "gone" }));
      return;
    }
    if (error.code === "chat_queue_full") {
      const attempt = (this.retries.get(paneId) ?? 0) + 1;
      this.retries.set(paneId, attempt);
      if (attempt < OPEN_ATTEMPTS) {
        const delay = (this.options.retryDelayMs ?? 500) * attempt;
        (this.options.setTimer ?? ((ms, run) => setTimeout(run, ms)))(delay, () => this.open(paneId));
        return;
      }
    }
    this.log(`chat.open.refused pane=${paneId} code=${error.code}`);
    this.update(paneId, (pane) => ({ ...pane, status: "error", error: error.message || "This chat can't be opened." }));
  }

  onEvent(event: ChatEvent): void {
    const pane = this.pane(event.paneId);
    if (!pane || !this.holders.has(event.paneId) || pane.status === "gone") return;
    const outcome = applyChatEvent(pane, event);
    this.put(outcome.pane);
    if (event.change.case === "snapshot") this.resendUnsure(event.paneId);
    if (outcome.reopen) {
      this.log(`chat.resync pane=${event.paneId} held=${pane.revision ?? "none"} got=${event.revision} change=${event.change.case ?? "none"}`);
      this.open(event.paneId);
    }
    if (outcome.page) this.takePage(event.paneId, outcome.page);
  }

  /** Every reconnect is a new connection: subscriptions are per connection. */
  onConnected(): void {
    this.opening.clear();
    this.reopenWanted.clear();
    const current = this.options.serverIdentity();
    for (const paneId of this.holders.keys()) {
      const pane = this.pane(paneId);
      if (!pane || pane.status === "gone") continue;
      // Another tmux server (a restart, or another host): the same pane id names another pane.
      if (pane.serverIdentity && current && pane.serverIdentity !== current) {
        this.put({ ...pane, status: "gone", resyncing: false, loadingOlder: false });
        continue;
      }
      // What the phone holds stays on screen until the snapshot replaces it.
      this.put({ ...pane, resyncing: true, loadingOlder: false, pendingPage: undefined, status: pane.status === "error" ? "opening" : pane.status, serverIdentity: current || pane.serverIdentity });
      this.open(paneId);
    }
  }

  /**
   * A topology snapshot: a held chat whose pane is gone has ended. CLOSE lets
   * the host forget it. An empty set is a transient snapshot, not every pane
   * closing.
   */
  reconcilePanes(serverIdentity: string, livePaneIds: ReadonlySet<string>): void {
    if (livePaneIds.size === 0) return;
    for (const paneId of this.holders.keys()) {
      const pane = this.pane(paneId);
      if (!pane || pane.status === "gone" || pane.serverIdentity !== serverIdentity || livePaneIds.has(paneId)) continue;
      this.fireAndForget(chatClose(this.target(pane)), "close", paneId);
      this.update(paneId, (held) => ({ ...held, status: "gone" }));
    }
  }

  /** The tmux server behind the connection was replaced: pane ids now name other panes. */
  onServerChanged(serverIdentity: string): void {
    for (const paneId of this.holders.keys()) {
      this.update(paneId, (pane) => (pane.serverIdentity === serverIdentity ? pane : { ...pane, status: "gone" }));
    }
  }

  /**
   * The connection is going and the phone forgets the host. Screens still
   * mounted show nothing to follow. Each pane keeps the server it was on, so
   * the next connection reopens it only on that server.
   */
  reset(): void {
    this.opening.clear();
    this.reopenWanted.clear();
    for (const paneId of Object.keys(this.options.store.getState().panes)) this.dropUnsure(paneId);
    this.retries.clear();
    this.options.store.setState((state) => ({
      ...state,
      panes: Object.fromEntries(Object.entries(state.panes).map(([paneId, pane]) => [paneId, newChatPane(paneId, pane.serverIdentity)])),
    }));
  }

  // ---- commands ----------------------------------------------------------------

  /** Sends `text`; it shows at once and is replaced by the host's own item. */
  /**
   * Sends `text`; it shows at once and is replaced by the host's own item. A
   * refusal throws and takes it off screen. A lost answer does not throw: the
   * message stays and is sent again, with the same id, after the next snapshot.
   * `attachments` are files already in the host's upload folder; with any,
   * `text` may be empty.
   */
  async send(paneId: string, text: string, attachments: readonly OutgoingAttachment[] = []): Promise<void> {
    const pane = this.requirePane(paneId);
    if (!this.live()) throw new Error("Not connected.");
    const id = newClientMessageId();
    const message: OutgoingMessage = { id, text, acked: false, ...(attachments.length > 0 ? { attachments } : {}) };
    this.update(paneId, (held) => ({ ...held, outgoing: [...held.outgoing, message] }));
    await this.deliver(pane, id, text, true, attachments);
  }

  /** One SEND of an outgoing message. */
  private async deliver(pane: ChatPane, id: string, text: string, first: boolean, attachments: readonly OutgoingAttachment[] = []): Promise<void> {
    const paneId = pane.paneId;
    const mark = (change: (message: OutgoingMessage) => OutgoingMessage | undefined) =>
      this.update(paneId, (held) => ({ ...held, outgoing: held.outgoing.flatMap((message) => (message.id === id ? (change(message) ?? []) : [message])) }));
    try {
      await this.request(chatSend(this.target(pane), text, id, attachments));
    } catch (error) {
      if (error instanceof HostError && error.code === "chat_message_duplicate") {
        // The first send of this id arrived; the answer to it was lost. Sent
        // again after a snapshot that lacks it, the item is older than the
        // snapshot's window and no event will replace the placeholder.
        if (!first) {
          mark(() => undefined);
          return;
        }
      } else if (error instanceof HostError) {
        mark(() => undefined);
        if (first) throw error;
        this.options.onUnconfirmed?.(paneId, text, error);
        return;
      } else {
        this.log(`chat.send.unsure pane=${paneId} ${error instanceof Error ? error.name : "error"}`);
        mark((message) => ({ ...message, unsure: true }));
        // Unanswered on a live connection: a fresh snapshot settles it. A
        // dropped connection reopens on its own. Only the first try asks, so
        // a host that keeps not answering is not asked in a loop.
        if (first && this.live()) this.open(paneId);
        return;
      }
    }
    mark((message) => ({ ...message, acked: true, unsure: false }));
  }

  /** Tells the user about messages still unconfirmed when the pane leaves the screen. */
  private dropUnsure(paneId: string): void {
    for (const message of this.pane(paneId)?.outgoing ?? []) {
      if (message.unsure) this.options.onUnconfirmed?.(paneId, message.text);
    }
  }

  /** After a snapshot: messages whose SEND answer was lost and that the timeline lacks go again. */
  private resendUnsure(paneId: string): void {
    const pane = this.pane(paneId);
    if (!pane) return;
    for (const message of pane.outgoing) {
      if (!message.unsure) continue;
      this.update(paneId, (held) => ({ ...held, outgoing: held.outgoing.map((entry) => (entry.id === message.id ? { ...entry, unsure: false } : entry)) }));
      this.log(`chat.send.again pane=${paneId}`);
      void this.deliver(pane, message.id, message.text, false, message.attachments);
    }
  }

  /** Edit-and-resend: drop the user message `itemId` and what follows, then send `text` in its place, with `attachments`. */
  async rewindAndSend(paneId: string, itemId: string, text: string, attachments: readonly OutgoingAttachment[] = []): Promise<void> {
    await this.request(chatRewind(this.target(this.requirePane(paneId)), itemId));
    await this.send(paneId, text, attachments);
  }

  async interrupt(paneId: string): Promise<void> {
    await this.request(chatInterrupt(this.target(this.requirePane(paneId))));
  }

  async answer(paneId: string, requestId: string, answer: MessageInitShape<typeof ChatAnswerSchema>): Promise<void> {
    await this.request(chatAnswer(this.target(this.requirePane(paneId)), requestId, answer));
  }

  async setOptions(paneId: string, options: MessageInitShape<typeof ChatOptionsSchema>): Promise<void> {
    await this.request(chatSetOptions(this.target(this.requirePane(paneId)), options));
  }

  /** `/btw` and `/side`: answers the pane the fork opened in. */
  async sideFork(paneId: string, question: string): Promise<string> {
    const response = await this.request(chatSideFork(this.target(this.requirePane(paneId)), question));
    return response.chat?.forkedPaneId ?? "";
  }

  /**
   * Fork from here: the conversation through the end of the turn `itemId` is
   * in, as a new chat. Answers the pane the fork opened in; a refusal throws.
   */
  async fork(paneId: string, itemId: string): Promise<string> {
    const response = await this.request(chatFork(this.target(this.requirePane(paneId)), itemId));
    return response.chat?.forkedPaneId ?? "";
  }

  /** Stops one of the pane's background tasks (`ChatState.background`). It leaves the list by a later state event. */
  async stopTask(paneId: string, taskId: string): Promise<void> {
    await this.request(chatStopTask(this.target(this.requirePane(paneId)), taskId));
  }

  /** The page before what the pane holds. */
  async loadOlder(paneId: string): Promise<void> {
    const pane = this.pane(paneId);
    if (!pane || pane.status !== "open" || pane.loadingOlder || pane.olderCursor === "") return;
    const snapshotSeq = pane.snapshotSeq;
    const cursor = pane.olderCursor;
    const hold = this.holds.get(paneId);
    this.update(paneId, (held) => ({ ...held, loadingOlder: true }));
    let response: Response;
    try {
      response = await this.request(chatLoadOlder(this.target(pane), cursor));
    } catch (error) {
      if (this.holds.get(paneId) !== hold) return;
      this.update(paneId, (held) => ({ ...held, loadingOlder: false }));
      // The cursor is from before a rewind or a trim: a fresh snapshot has the current one.
      if (error instanceof HostError && error.code === "chat_cursor_invalid") this.open(paneId);
      else throw error;
      return;
    }
    // The screen let go and came back: this page answers the earlier hold.
    if (this.holds.get(paneId) !== hold) return;
    const chat = response.chat;
    this.takePage(paneId, { snapshotSeq, cursor, revision: chat?.revision ?? 0n, older: chat?.older ?? [], olderCursor: chat?.olderCursor ?? "" });
  }

  private takePage(paneId: string, page: OlderPage): void {
    const pane = this.pane(paneId);
    if (!pane) return;
    const outcome = applyOlderPage(pane, page);
    this.put(outcome.pane);
    if (outcome.kind === "refetch") {
      this.update(paneId, (held) => ({ ...held, loadingOlder: false }));
      void this.loadOlder(paneId).catch(() => undefined);
    }
  }

  // ---- helpers -------------------------------------------------------------------

  private target(pane: ChatPane): ChatTarget {
    return { paneId: pane.paneId, expectedServerIdentity: pane.serverIdentity || this.options.serverIdentity() };
  }

  private requirePane(paneId: string): ChatPane {
    const pane = this.pane(paneId);
    if (!pane) throw new Error("This chat is not open.");
    return pane;
  }

  private live(): ChatConnection | null {
    const connection = this.options.getConnection();
    return connection && connection.state === "connected" ? connection : null;
  }

  private request(request: Request): Promise<Response> {
    const connection = this.live();
    if (!connection) return Promise.reject(new Error("Not connected."));
    return connection.request(request);
  }

  private fireAndForget(request: Request, what: string, paneId: string): void {
    const connection = this.live();
    if (!connection) return;
    connection.request(request).catch((error: unknown) => {
      this.log(`chat.${what}.failed pane=${paneId} ${error instanceof HostError ? error.code : error instanceof Error ? error.name : "error"}`);
    });
  }

  private put(pane: ChatPane): void {
    this.options.store.setState((state) => ({ ...state, panes: { ...state.panes, [pane.paneId]: pane } }));
  }

  private update(paneId: string, change: (pane: ChatPane) => ChatPane): void {
    const pane = this.pane(paneId);
    if (pane) this.put(change(pane));
  }

  private remove(paneId: string): void {
    this.options.store.setState((state) => {
      const panes = { ...state.panes };
      delete panes[paneId];
      return { ...state, panes };
    });
  }

  private log(line: string): void {
    this.options.log?.(`[muxflow] ${line}`);
  }
}

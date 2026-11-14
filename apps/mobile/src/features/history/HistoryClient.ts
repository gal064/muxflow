// Session history and snooze (docs/design/chat-plan.md §4, history.proto).
// The phone lists when the History screen opens and again after every
// SESSION_HISTORY event while it is open; search is local.

import { createStore, type StoreApi } from "zustand/vanilla";
import type { Request, Response } from "../../protocol/gen/envelope_pb";
import type { HistoryEntry, HistoryEvent } from "../../protocol/gen/history_pb";
import { historyList, historyResume, historySnooze, historyWake, type HistoryTarget, type HistoryTopology } from "../../protocol/requests";

export interface HistoryState {
  entries: HistoryEntry[];
  generation: bigint;
  /** A LIST has answered since the screen opened. */
  loaded: boolean;
  loading: boolean;
  /** Why the last LIST failed; empty when it did not. */
  error: string;
}

export type HistoryStore = StoreApi<HistoryState>;

export function createHistoryStore(): HistoryStore {
  return createStore<HistoryState>(() => ({ entries: [], generation: 0n, loaded: false, loading: false, error: "" }));
}

export interface HistoryConnection {
  readonly state: string;
  request(request: Request): Promise<Response>;
}

export interface HistoryClientOptions {
  store: HistoryStore;
  getConnection: () => HistoryConnection | null;
  topology: () => HistoryTopology;
  /** A snooze ended: say which, by title. */
  onWoke?: (entries: HistoryEntry[]) => void;
  log?: (line: string) => void;
}

/** Where RESUME opened the session. */
export interface ResumedAt {
  sessionId: string;
  windowId: string;
  paneId: string;
}

export class HistoryClient {
  private watchers = 0;
  private listing: Promise<void> | undefined;
  private listAgain = false;

  constructor(private readonly options: HistoryClientOptions) {}

  get store(): HistoryStore {
    return this.options.store;
  }

  /** The History screen is open: list now and after every change. Returns its release. */
  watch(): () => void {
    this.watchers += 1;
    void this.list();
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.watchers -= 1;
    };
  }

  onEvent(event: HistoryEvent): void {
    if (event.woke.length > 0) this.options.onWoke?.(event.woke);
    if (this.watchers > 0 && event.generation !== this.options.store.getState().generation) void this.list();
  }

  onConnected(): void {
    if (this.watchers > 0) void this.list();
  }

  reset(): void {
    this.options.store.setState({ entries: [], generation: 0n, loaded: false, loading: false, error: "" });
  }

  /** One LIST at a time; a list asked for meanwhile runs once after it. */
  list(): Promise<void> {
    if (this.listing) {
      this.listAgain = true;
      return this.listing;
    }
    this.listing = this.runList().finally(() => {
      this.listing = undefined;
      if (this.listAgain) {
        this.listAgain = false;
        void this.list();
      }
    });
    return this.listing;
  }

  private async runList(): Promise<void> {
    const connection = this.live();
    if (!connection) return;
    this.options.store.setState({ loading: true });
    try {
      const response = await connection.request(historyList());
      const history = response.history;
      this.options.store.setState({ entries: history?.entries ?? [], generation: history?.generation ?? 0n, loaded: true, loading: false, error: "" });
    } catch (error) {
      this.options.log?.(`[muxflow] history.list.failed ${error instanceof Error ? error.name : "error"}`);
      this.options.store.setState({ loading: false, loaded: true, error: error instanceof Error ? error.message : String(error) });
    }
  }

  async resume(target: HistoryTarget): Promise<ResumedAt> {
    const response = await this.request(historyResume(target, this.options.topology()));
    const history = response.history;
    return { sessionId: history?.sessionId ?? "", windowId: history?.windowId ?? "", paneId: history?.paneId ?? "" };
  }

  async snooze(target: HistoryTarget, untilMs: number): Promise<void> {
    await this.request(historySnooze(target, BigInt(Math.round(untilMs)), this.options.topology()));
    if (this.watchers > 0) void this.list();
  }

  async wake(target: HistoryTarget): Promise<void> {
    await this.request(historyWake(target));
    if (this.watchers > 0) void this.list();
  }

  private live(): HistoryConnection | null {
    const connection = this.options.getConnection();
    return connection && connection.state === "connected" ? connection : null;
  }

  private request(request: Request): Promise<Response> {
    const connection = this.live();
    if (!connection) return Promise.reject(new Error("Not connected."));
    return connection.request(request);
  }
}

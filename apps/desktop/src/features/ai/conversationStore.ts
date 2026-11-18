import type { AiAction, AiApproval, AiContinuation, AiTurn } from "./types";

/** What the panel renders for one conversation. */
export interface AiHostConversation {
  turns: AiTurn[];
  /** A generation is in flight for this conversation. */
  streaming: boolean;
  /** The last failure; the next send clears it. */
  error?: string;
  /** What the assistant is doing between deltas, e.g. `calling list_panes`. */
  activity?: string;
  /** Tool calls waiting on the user's Allow/Deny, oldest first. */
  pending: readonly AiApproval[];
  /** The round budget is spent; the run waits here for Continue or Stop. */
  continuation?: AiContinuation;
  /**
   * Identifies this question's run. Clearing, pruning or reopening a window
   * moves the generation, and a loop still holding the old one stops touching
   * the conversation — and stops its own network call.
   */
  generation: number;
  /**
   * How to stop the in-flight generation. It lives here rather than in the
   * panel because the panel unmounts when another tab is clicked, and the
   * Stop button has to survive that.
   */
  cancel?: () => void;
}

const EMPTY: AiHostConversation = { turns: [], streaming: false, pending: [], generation: 0 };


/**
 * Conversations live here rather than in the panel's own state because the
 * panel unmounts the moment another tab is clicked: a generation that was
 * mid-answer would otherwise lose its text, and the answer would arrive to
 * nobody. Keyed by host *and tmux window* — the strip's tab — so every tab
 * holds its own thread and switching tabs switches conversations. A closed
 * window's thread is pruned: window ids are reused, and an old answer about
 * a dead window in a new window's panel would be a lie.
 */
const conversations = new Map<string, AiHostConversation>();
const listeners = new Set<() => void>();

/** Four round trips per tab, kept short so the history cannot outgrow the model's window. */
const MAX_TURNS = 40;

let nextGeneration = 1;

function publish(): void {
  for (const listener of [...listeners]) listener();
}

/** Ends one entry for good: its generation moves, so any loop still holding
 * it stops, and anything it was waiting on is refused rather than left hung. */
function retire(key: string, entry: AiHostConversation): void {
  for (const approval of entry.pending) approval.decide(false);
  entry.continuation?.resume(false);
  entry.cancel?.();
  conversations.delete(key);
}

export function conversationKey(profileId: string, windowId: string): string {
  return `${profileId}\u0000${windowId}`;
}

export const aiConversations = {
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  },

  read(key: string): AiHostConversation {
    return conversations.get(key) ?? EMPTY;
  },

  /**
   * Opens a question and the empty answer that fills in under it. Returns the
   * turns that preceded it — the history the model gets — and the generation
   * this question owns; the loop checks it after every await and stops when
   * the conversation was cleared, pruned or reopened underneath it.
   */
  begin(key: string, question: string): { history: AiTurn[]; generation: number } {
    const current = conversations.get(key);
    if (current) retire(key, current);
    const generation = nextGeneration++;
    const history = current?.turns ?? [];
    const turns: AiTurn[] = [...history, { role: "user", text: question }, { role: "assistant", text: "" }];
    conversations.set(key, {
      turns: turns.slice(-MAX_TURNS),
      streaming: true,
      pending: [],
      generation,
    });
    publish();
    return { history, generation };
  },

  /** Appends to the open answer; ignored after `end`, e.g. by a late delta. */
  delta(key: string, text: string): void {
    const current = conversations.get(key);
    if (!current?.streaming) return;
    const turns = [...current.turns];
    const last = turns[turns.length - 1];
    if (last?.role !== "assistant") return;
    turns[turns.length - 1] = { ...last, text: last.text + text };
    conversations.set(key, { ...current, turns });
    publish();
  },

  /** Records what the assistant did, under the answer it did it for. */
  addAction(key: string, action: AiAction): void {
    const current = conversations.get(key);
    if (!current?.streaming) return;
    const turns = [...current.turns];
    const last = turns[turns.length - 1];
    if (last?.role !== "assistant") return;
    turns[turns.length - 1] = { ...last, actions: [...(last.actions ?? []), action] };
    conversations.set(key, { ...current, turns });
    publish();
  },

  /** What the assistant is doing between deltas; `undefined` clears it. */
  setActivity(key: string, activity: string | undefined): void {
    const current = conversations.get(key);
    if (!current?.streaming || current.activity === activity) return;
    conversations.set(key, { ...current, activity });
    publish();
  },

  /** Hands over the means to stop, once the current round's call exists. */
  attach(key: string, cancel: () => void): void {
    const current = conversations.get(key);
    if (!current?.streaming) return;
    conversations.set(key, { ...current, cancel });
  },

  /** Parks a tool call on the conversation until the user decides. */
  addApproval(key: string, approval: AiApproval): void {
    const current = conversations.get(key);
    if (!current?.streaming) {
      approval.decide(false);
      return;
    }
    conversations.set(key, { ...current, pending: [...current.pending, approval] });
    publish();
  },

  /** The panel's Allow/Deny. Unknown or already-answered ids are ignored. */
  resolveApproval(key: string, id: string, approved: boolean): void {
    const current = conversations.get(key);
    if (!current) return;
    const approval = current.pending.find((candidate) => candidate.id === id);
    if (!approval) return;
    conversations.set(key, { ...current, pending: current.pending.filter((candidate) => candidate.id !== id) });
    publish();
    approval.decide(approved);
  },

  /** Parks a spent round budget on the conversation until Continue or Stop. */
  addContinuation(key: string, continuation: AiContinuation): void {
    const current = conversations.get(key);
    if (!current?.streaming) {
      continuation.resume(false);
      return;
    }
    conversations.set(key, { ...current, continuation });
    publish();
  },

  /** The user chose Continue; the card goes away and the loop resumes. */
  clearContinuation(key: string): void {
    const current = conversations.get(key);
    if (!current?.continuation) return;
    conversations.set(key, { ...current, continuation: undefined });
    publish();
  },

  /** Ends the generation — answered, cancelled or failed. Idempotent. */
  end(key: string, error?: string): void {
    const current = conversations.get(key);
    if (!current || !current.streaming) return;
    for (const approval of current.pending) approval.decide(false);
    current.continuation?.resume(false);
    conversations.set(key, {
      ...current, streaming: false, pending: [], continuation: undefined, cancel: undefined, activity: undefined, error,
    });
    publish();
  },

  clear(key: string): void {
    const current = conversations.get(key);
    if (!current) return;
    retire(key, current);
    publish();
  },

  /**
   * Drops this host's conversations whose window has closed. The live ids
   * come from the host's current snapshot; an empty live set while the host
   * is connected still means every one of its windows is gone.
   */
  pruneHost(profileId: string, liveWindowIds: ReadonlySet<string>): void {
    let changed = false;
    const prefix = `${profileId}\u0000`;
    for (const [key, entry] of [...conversations]) {
      if (!key.startsWith(prefix)) continue;
      if (liveWindowIds.has(key.slice(prefix.length))) continue;
      retire(key, entry);
      changed = true;
    }
    if (changed) publish();
  },

  /** Everything, for tests and for a settings reset. */
  clearAll(): void {
    for (const [key, entry] of [...conversations]) retire(key, entry);
    publish();
  },
};

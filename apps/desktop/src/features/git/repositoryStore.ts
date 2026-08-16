import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import { recordPerfCounter, recordPerfHighWater } from "../../perf/probe";
import type {
  GitDiffResult,
  GitDiffTarget,
  GitStatusSnapshot,
  GitWatchLease,
  GitWorkspaceClient,
  GitWorkspaceEvent,
} from "./types";

/**
 * One shared observation of one repository, for however many consumers.
 *
 * The sidebar and every matching diff tab used to each open their own watch,
 * each of which cost the host a discovery, a native watcher and a status
 * pipeline. They share one here: the second consumer of a repository pays no
 * round trip at all, and sees the current status the moment it acquires.
 */
export interface GitRepositoryState {
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
}

export interface GitRepositoryHandle {
  /** The current shared state; safe to read during render. */
  state(): GitRepositoryState;
  subscribe(listener: () => void): () => void;
  /** Re-reads status through the shared entry, coalescing with any in flight. */
  refresh(): Promise<void>;
  /** Adopts an authoritative status the caller already has, e.g. a mutation's. */
  accept(status: GitStatusSnapshot): void;
  /** The diff for one entry, reusing an identical in-flight or cached result. */
  diff(request: GitDiffRequest): Promise<GitDiffResult>;
  release(): void;
}

export interface GitDiffRequest {
  repositoryId: string;
  path: string;
  originalPath?: string;
  target: GitDiffTarget;
}

/** Distinct results kept per repository. One per open diff tab plus headroom. */
const MAX_CACHED_DIFFS = 16;

/** Repositories whose last status is remembered after their last consumer. */
const MAX_REMEMBERED_STATUSES = 8;

/** Watch events held while a bootstrap response is still in flight. */
const MAX_PENDING_EVENTS = 64;

function scopeKey(scope: FileWorkspaceScope, root: ActiveRoot): string {
  return [scope.clientId, scope.serverIdentity, scope.terminalEpoch, root.token, root.path].join("\0");
}

function diffKey(request: GitDiffRequest, generation: string): string {
  return [request.repositoryId, request.path, request.originalPath ?? "", request.target, generation].join("\0");
}

/**
 * One shared entry. Its lifetime is exactly the span during which at least one
 * consumer holds it, so a final release leaves no watch and no task behind.
 */
class RepositoryEntry {
  readonly #client: GitWorkspaceClient;
  readonly #scope: FileWorkspaceScope;
  readonly #root: ActiveRoot;
  readonly #listeners = new Set<() => void>();
  readonly #diffs = new Map<string, Promise<GitDiffResult>>();
  readonly #onEmpty: () => void;
  #consumers = 0;
  #state: GitRepositoryState = { loading: true };
  #lease: GitWatchLease | undefined;
  #stopEvents: (() => void) | undefined;
  #abort = new AbortController();
  #refreshing: Promise<void> | undefined;
  #disposed = false;
  /**
   * Events that arrived before the watch lease resolved.
   *
   * The host activates a watch only after its bootstrap response is enqueued,
   * but the response and the event stream reach the renderer independently, so
   * a refresh can be observed first. Bounded because a wedged bootstrap must
   * not accumulate them without limit.
   */
  #pending: GitWorkspaceEvent[] = [];

  constructor(client: GitWorkspaceClient, scope: FileWorkspaceScope, root: ActiveRoot, remembered: GitStatusSnapshot | undefined, onEmpty: () => void) {
    this.#client = client;
    this.#scope = scope;
    this.#root = root;
    this.#onEmpty = onEmpty;
    // A remembered snapshot paints immediately while the watch is re-acquired,
    // so reopening a panel is never a blank surface.
    if (remembered) this.#state = { status: remembered, loading: true };
    this.#start();
  }

  get state(): GitRepositoryState {
    return this.#state;
  }

  get lastStatus(): GitStatusSnapshot | undefined {
    return this.#state.status;
  }

  acquire(): void {
    this.#consumers += 1;
    recordPerfHighWater("git.sharedRepositoryConsumers", this.#consumers);
  }

  release(): void {
    this.#consumers -= 1;
    if (this.#consumers > 0) return;
    this.#dispose();
    this.#onEmpty();
  }

  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => { this.#listeners.delete(listener); };
  }

  accept(status: GitStatusSnapshot): void {
    if (this.#disposed) return;
    const current = this.#state.status;
    if (current && current.repository.id === status.repository.id) {
      // Equal state is not a transition. Bailing here is what stops a mutation
      // that already delivered its status in its response from re-rendering
      // every row again when the shared watch echoes the same snapshot.
      if (current.generation === status.generation && current.sourceGeneration === status.sourceGeneration) return;
      if (BigInt(status.generation) < BigInt(current.generation)) return;
    }
    this.#publish({ status, loading: false });
  }

  async refresh(): Promise<void> {
    if (this.#disposed) return;
    if (this.#refreshing) return this.#refreshing;
    this.#publish({ ...this.#state, loading: true, error: undefined });
    const inFlight = (async () => {
      try {
        const status = await this.#client.status(this.#scope, this.#root, this.#abort.signal);
        this.accept(status);
      } catch (cause) {
        if (!this.#disposed && !this.#abort.signal.aborted) {
          this.#publish({ ...this.#state, loading: false, error: String(cause) });
        }
      } finally {
        this.#refreshing = undefined;
      }
    })();
    this.#refreshing = inFlight;
    return inFlight;
  }

  diff(request: GitDiffRequest): Promise<GitDiffResult> {
    const generation = this.#state.status?.generation ?? "0";
    const key = diffKey(request, generation);
    const existing = this.#diffs.get(key);
    if (existing) {
      recordPerfCounter("git.diffReuses");
      return existing;
    }
    const pending = this.#client
      .diff(this.#scope, this.#root, request.repositoryId, request.path, request.originalPath, request.target, this.#abort.signal)
      .then((result) => {
        this.accept(result.status);
        return result;
      })
      .catch((cause) => {
        // A failure is never cached: the next attempt must reach the host.
        this.#diffs.delete(key);
        throw cause;
      });
    this.#diffs.set(key, pending);
    this.#evictStaleDiffs(generation);
    return pending;
  }

  #evictStaleDiffs(generation: string): void {
    for (const key of [...this.#diffs.keys()]) {
      if (!key.endsWith(`\0${generation}`)) this.#diffs.delete(key);
    }
    while (this.#diffs.size > MAX_CACHED_DIFFS) {
      const oldest = this.#diffs.keys().next();
      if (oldest.done) break;
      this.#diffs.delete(oldest.value);
    }
  }

  #start(): void {
    this.#stopEvents = this.#client.subscribe((event) => {
      if (this.#disposed || event.rootToken !== this.#root.token) return;
      if (!this.#lease) {
        this.#pending.push(event);
        if (this.#pending.length > MAX_PENDING_EVENTS) this.#pending.shift();
        return;
      }
      // Events for a watch this entry does not own belong to another scope.
      if (event.watchId !== this.#lease.watchId) return;
      this.#apply(event);
    });
    void this.#client.watch(this.#scope, this.#root, this.#abort.signal).then((lease) => {
      if (this.#disposed || lease.rootToken !== this.#root.token || lease.connectionEpoch !== this.#scope.terminalEpoch) {
        lease.release();
        return;
      }
      this.#lease = lease;
      this.accept(lease.status);
      for (const event of this.#pending) {
        if (event.watchId === lease.watchId) this.#apply(event);
      }
      this.#pending = [];
      this.#publish({ ...this.#state, loading: false });
    }).catch((cause) => {
      if (this.#disposed || this.#abort.signal.aborted) return;
      this.#publish({ ...this.#state, loading: false, error: String(cause) });
    });
  }

  #apply(event: GitWorkspaceEvent): void {
    if (event.kind === "error") this.#publish({ ...this.#state, loading: false, error: event.error });
    else this.accept(event.status);
  }

  #dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#abort.abort();
    this.#stopEvents?.();
    this.#stopEvents = undefined;
    this.#lease?.release();
    this.#lease = undefined;
    this.#diffs.clear();
    this.#pending = [];
    this.#listeners.clear();
  }

  #publish(next: GitRepositoryState): void {
    this.#state = next;
    for (const listener of [...this.#listeners]) listener();
  }
}

/**
 * The registry of shared repository observations for one Git client.
 *
 * Keyed by connection, server identity, connection epoch, root token and root
 * path: anything narrower would let a reconnected or re-rooted consumer inherit
 * an observation it can no longer address.
 */
export class GitRepositoryStore {
  readonly #client: GitWorkspaceClient;
  readonly #entries = new Map<string, RepositoryEntry>();
  readonly #remembered = new Map<string, GitStatusSnapshot>();

  constructor(client: GitWorkspaceClient) {
    this.#client = client;
  }

  acquire(scope: FileWorkspaceScope, root: ActiveRoot): GitRepositoryHandle {
    const key = scopeKey(scope, root);
    let entry = this.#entries.get(key);
    if (entry) {
      recordPerfCounter("git.sharedRepositoryReuses");
    } else {
      recordPerfCounter("git.sharedRepositoryStarts");
      entry = new RepositoryEntry(this.#client, scope, root, this.#remembered.get(key), () => {
        const status = this.#entries.get(key)?.lastStatus;
        this.#entries.delete(key);
        if (status) this.#remember(key, status);
      });
      this.#entries.set(key, entry);
    }
    entry.acquire();
    const owner = entry;
    let released = false;
    return {
      state: () => owner.state,
      subscribe: (listener) => owner.subscribe(listener),
      refresh: () => owner.refresh(),
      accept: (status) => owner.accept(status),
      diff: (request) => owner.diff(request),
      release: () => {
        if (released) return;
        released = true;
        owner.release();
      },
    };
  }

  /**
   * The shared state for a scope without acquiring it.
   *
   * Rendering may read this before an effect has run, so a consumer mounting
   * beside a live observation paints that repository's status on its first
   * render instead of a spinner it does not need.
   */
  peek(scope: FileWorkspaceScope, root: ActiveRoot): GitRepositoryState | undefined {
    const key = scopeKey(scope, root);
    const entry = this.#entries.get(key);
    if (entry) return entry.state;
    const remembered = this.#remembered.get(key);
    return remembered ? { status: remembered, loading: true } : undefined;
  }

  #remember(key: string, status: GitStatusSnapshot): void {
    this.#remembered.delete(key);
    this.#remembered.set(key, status);
    while (this.#remembered.size > MAX_REMEMBERED_STATUSES) {
      const oldest = this.#remembered.keys().next();
      if (oldest.done) break;
      this.#remembered.delete(oldest.value);
    }
  }
}

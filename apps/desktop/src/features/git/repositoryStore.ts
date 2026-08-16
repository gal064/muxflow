import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import { recordPerfCounter, recordPerfHighWater } from "../../perf/probe";
import type {
  GitCommandResult,
  GitDiffResult,
  GitDiffTarget,
  GitMutationRequest,
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
 *
 * Mutations go through here too, so the authoritative status a command returns
 * is reconciled once, in the one place that owns this repository's state.
 */
export interface GitRepositoryState {
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
}

export interface GitRepositoryHandle {
  /** The current shared state. A stable object between publications. */
  state(): GitRepositoryState;
  subscribe(listener: () => void): () => void;
  /** An explicit re-read. The host answers this one without its watch cache. */
  refresh(): Promise<void>;
  /** Adopts an authoritative status the caller already has. */
  accept(status: GitStatusSnapshot): void;
  /** The diff for one entry, joining an identical request already in flight. */
  diff(request: GitDiffRequest, signal?: AbortSignal): Promise<GitDiffResult>;
  /** One mutation, whose returned authoritative status is reconciled here. */
  mutate(repositoryId: string, request: GitMutationRequest): Promise<GitCommandResult>;
  /** Mints the one-time host token a discard requires. */
  prepareDiscard(repositoryId: string, request: GitMutationRequest): Promise<string>;
  commit(repositoryId: string, expectedStatusGeneration: string, message: string): Promise<GitCommandResult>;
  release(): void;
}

export interface GitDiffRequest {
  repositoryId: string;
  path: string;
  originalPath?: string;
  target: GitDiffTarget;
}

/** Repositories whose last status is remembered after their last consumer. */
const MAX_REMEMBERED_STATUSES = 8;

/** Watch events held while a bootstrap response is still in flight. */
const MAX_PENDING_EVENTS = 64;

const NOT_OBSERVED: GitRepositoryState = { loading: true };

function scopeKey(scope: FileWorkspaceScope, root: ActiveRoot): string {
  return [scope.clientId, scope.serverIdentity, scope.terminalEpoch, root.token, root.path].join("\0");
}

function diffKey(request: GitDiffRequest): string {
  return [request.repositoryId, request.path, request.originalPath ?? "", request.target].join("\0");
}

/**
 * One diff request that more than one caller may be waiting on.
 *
 * Joiners are counted rather than assumed, because cancellation belongs to the
 * caller: a tab that closes must stop its own read — including the bulk body
 * stream, which is why this exists at all — without cancelling a peer that is
 * still waiting for the same bytes.
 */
class SharedDiffRequest {
  readonly promise: Promise<GitDiffResult>;
  readonly #controller = new AbortController();
  #joiners = 0;

  constructor(start: (signal: AbortSignal) => Promise<GitDiffResult>) {
    this.promise = start(this.#controller.signal);
    // Nothing else observes this promise, and an unobserved rejection is a
    // console error the user cannot act on.
    this.promise.catch(() => undefined);
  }

  join(signal: AbortSignal | undefined, onIdle: () => void): Promise<GitDiffResult> {
    this.#joiners += 1;
    let departed = false;
    const depart = () => {
      if (departed) return;
      departed = true;
      this.#joiners -= 1;
      if (this.#joiners > 0) return;
      this.#controller.abort();
      onIdle();
    };
    signal?.addEventListener("abort", depart, { once: true });
    return this.promise.finally(() => {
      signal?.removeEventListener("abort", depart);
      depart();
    });
  }

  abandon(): void {
    this.#controller.abort();
  }
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
  readonly #diffs = new Map<string, SharedDiffRequest>();
  readonly #onEmpty: () => void;
  #consumers = 0;
  #state: GitRepositoryState = NOT_OBSERVED;
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
    const settled = this.#state.loading || this.#state.error !== undefined;
    if (current && current.repository.id === status.repository.id) {
      // Equal state is not a status transition. Bailing here is what stops a
      // mutation that already delivered its status in its response from
      // re-rendering every row when the shared watch echoes the same snapshot.
      // Settling still has to happen: an identical snapshot is the normal
      // answer to an explicit refresh, and it is also how an error clears.
      const identical = current.generation === status.generation
        && current.sourceGeneration === status.sourceGeneration;
      if (identical || BigInt(status.generation) < BigInt(current.generation)) {
        if (settled) this.#publish({ status: current, loading: false });
        return;
      }
    }
    this.#publish({ status, loading: false });
  }

  async refresh(): Promise<void> {
    if (this.#disposed) return;
    if (this.#refreshing) return this.#refreshing;
    this.#publish({ ...this.#state, loading: true, error: undefined });
    const inFlight = (async () => {
      try {
        this.accept(await this.#client.status(this.#scope, this.#root, this.#abort.signal));
      } catch (cause) {
        if (this.#disposed || this.#abort.signal.aborted) return;
        this.#publish({ ...this.#state, loading: false, error: String(cause) });
      } finally {
        this.#refreshing = undefined;
        // A refresh that changed nothing still has to stop saying it is
        // loading; `accept` deliberately suppresses the state transition.
        if (!this.#disposed && this.#state.loading) {
          this.#publish({ ...this.#state, loading: false });
        }
      }
    })();
    this.#refreshing = inFlight;
    return inFlight;
  }

  /**
   * The diff for one entry.
   *
   * Only requests actually in flight are shared. A resolved diff is not cached:
   * the host is the authority on what a file currently looks like, and an
   * explicit refresh that answered from memory would be no refresh at all.
   */
  diff(request: GitDiffRequest, signal?: AbortSignal): Promise<GitDiffResult> {
    if (this.#disposed) return Promise.reject(new Error("This repository is no longer observed."));
    const key = diffKey(request);
    const existing = this.#diffs.get(key);
    if (existing) {
      recordPerfCounter("git.diffReuses");
      return existing.join(signal, () => this.#diffs.delete(key));
    }
    const shared = new SharedDiffRequest((requestSignal) => this.#client
      .diff(this.#scope, this.#root, request.repositoryId, request.path, request.originalPath, request.target, requestSignal)
      .then((result) => {
        this.accept(result.status);
        return result;
      }));
    this.#diffs.set(key, shared);
    return shared.join(signal, () => this.#diffs.delete(key));
  }

  async mutate(repositoryId: string, request: GitMutationRequest): Promise<GitCommandResult> {
    const result = await this.#client.mutate(this.#scope, this.#root, repositoryId, request);
    if (result.status) this.accept(result.status);
    return result;
  }

  prepareDiscard(repositoryId: string, request: GitMutationRequest): Promise<string> {
    return this.#client.prepareDiscard(this.#scope, this.#root, repositoryId, request);
  }

  async commit(repositoryId: string, expectedStatusGeneration: string, message: string): Promise<GitCommandResult> {
    const result = await this.#client.commit(this.#scope, this.#root, repositoryId, expectedStatusGeneration, message);
    if (result.status) this.accept(result.status);
    return result;
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
    for (const shared of this.#diffs.values()) shared.abandon();
    this.#diffs.clear();
    this.#stopEvents?.();
    this.#stopEvents = undefined;
    this.#lease?.release();
    this.#lease = undefined;
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
  readonly #remembered = new Map<string, GitRepositoryState>();

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
      entry = new RepositoryEntry(this.#client, scope, root, this.#remembered.get(key)?.status, () => {
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
      diff: (request, signal) => owner.diff(request, signal),
      mutate: (repositoryId, request) => owner.mutate(repositoryId, request),
      prepareDiscard: (repositoryId, request) => owner.prepareDiscard(repositoryId, request),
      commit: (repositoryId, expectedStatusGeneration, message) => owner.commit(repositoryId, expectedStatusGeneration, message),
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
   * render instead of a spinner it does not need. The returned object is stable
   * between publications, which is what makes it safe as a store snapshot.
   */
  peek(scope: FileWorkspaceScope, root: ActiveRoot): GitRepositoryState {
    const key = scopeKey(scope, root);
    return this.#entries.get(key)?.state ?? this.#remembered.get(key) ?? NOT_OBSERVED;
  }

  #remember(key: string, status: GitStatusSnapshot): void {
    this.#remembered.delete(key);
    this.#remembered.set(key, { status, loading: true });
    while (this.#remembered.size > MAX_REMEMBERED_STATUSES) {
      const oldest = this.#remembered.keys().next();
      if (oldest.done) break;
      this.#remembered.delete(oldest.value);
    }
  }
}

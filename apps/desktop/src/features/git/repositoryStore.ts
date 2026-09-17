import { abortable } from "../../transport/abortable";
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
/** Everything a consumer can do to one shared observation. */
export interface GitRepositoryHandle {
  /** The current shared state. A stable object between publications. */
  state(): GitRepositoryState;
  subscribe(listener: () => void): () => void;
  /** An explicit re-read. The host answers this one without its watch cache. */
  refresh(): Promise<void>;
  /** The diff for one entry, joining an identical request already in flight. */
  diff(request: GitDiffRequest, signal?: AbortSignal): Promise<GitDiffResult>;
  /** One mutation, whose returned authoritative status is reconciled here. */
  mutate(repositoryId: string, request: GitMutationRequest): Promise<GitCommandResult>;
  /** Mints the one-time host token a discard requires. */
  prepareDiscard(repositoryId: string, request: GitMutationRequest): Promise<string>;
  commit(repositoryId: string, expectedStatusGeneration: string, message: string): Promise<GitCommandResult>;
  /** Publishes the current branch to its configured upstream. */
  push(repositoryId: string, expectedStatusGeneration: string): Promise<GitCommandResult>;
}

/**
 * The published state of one repository.
 *
 * The handle travels with the state rather than beside it, so a consumer that
 * reads a snapshot can never pair one repository's status with another's
 * observation. It is absent exactly when nothing is observing this scope.
 */
export interface GitRepositoryState {
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
  handle?: GitRepositoryHandle;
}

/** One consumer's claim on a shared observation. */
export interface GitRepositoryLease {
  readonly handle: GitRepositoryHandle;
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

/**
 * Everything an entry's captured scope is allowed to differ by: nothing.
 *
 * The entry keeps the first acquirer's scope and root objects and uses them for
 * every later request, so the key has to name every field those requests read —
 * `hostProfileId` included, because the deferred body read is addressed by it.
 */
export function gitScopeKey(scope: FileWorkspaceScope, root: ActiveRoot): string {
  return [
    scope.clientId,
    scope.hostProfileId,
    scope.serverIdentity,
    scope.terminalEpoch,
    root.token,
    root.path,
  ].join("\0");
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

  /**
   * One caller's view of the shared request.
   *
   * A departing caller always sees its own `AbortError`, whether or not peers
   * remain: it asked to stop waiting. Only the last departure cancels the
   * request itself.
   */
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
    // `abortable` departs on the abort path; `finally` departs on the settle
    // path. `depart` is idempotent, so the two cannot double-count, and the
    // shared request is only cancelled by the last caller to leave either way.
    return abortable(this.promise, signal, depart, "Git diff was cancelled.").finally(depart);
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
  #state: GitRepositoryState;
  #lease: GitWatchLease | undefined;
  #stopEvents: (() => void) | undefined;
  #abort = new AbortController();
  #refreshing: Promise<void> | undefined;
  /**
   * The bootstrap attempt currently in flight, if any.
   *
   * Without it, an explicit refresh issued while a watch was still pending
   * would open a second watch and leak whichever lease lost the race — a host
   * subscriber and its coalesced pipeline that nothing would ever release.
   */
  #bootstrap: number | undefined;
  #attempts = 0;
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

  /** This entry's stable operations facade, published with every state. */
  readonly #handle: GitRepositoryHandle;

  constructor(client: GitWorkspaceClient, scope: FileWorkspaceScope, root: ActiveRoot, remembered: GitStatusSnapshot | undefined, onEmpty: () => void) {
    this.#client = client;
    this.#scope = scope;
    this.#root = root;
    this.#onEmpty = onEmpty;
    this.#handle = {
      state: () => this.#state,
      subscribe: (listener) => this.subscribe(listener),
      refresh: () => this.refresh(),
      diff: (request, signal) => this.diff(request, signal),
      mutate: (repositoryId, request) => this.mutate(repositoryId, request),
      prepareDiscard: (repositoryId, request) => this.prepareDiscard(repositoryId, request),
      commit: (repositoryId, generation, message) => this.commit(repositoryId, generation, message),
      push: (repositoryId, generation) => this.push(repositoryId, generation),
    };
    // A remembered snapshot paints immediately while the watch is re-acquired,
    // so reopening a panel is never a blank surface.
    this.#state = remembered
      ? { status: remembered, loading: true, handle: this.#handle }
      : { loading: true, handle: this.#handle };
    this.#start();
  }

  get state(): GitRepositoryState {
    return this.#state;
  }

  get handle(): GitRepositoryHandle {
    return this.#handle;
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
      // An older snapshot says nothing new about the repository, but it is
      // still an answer: whatever was waiting for it has stopped waiting.
      if (BigInt(status.generation) < BigInt(current.generation)) {
        this.#settle();
        return;
      }
      // The identical snapshot is not a status transition, and re-publishing it
      // would re-render every row for nothing — a mutation delivers its status
      // in its own response and the shared watch then echoes the same one. It
      // is still an answer, though: it settles `loading` and clears an error.
      const identical = current.generation === status.generation
        && current.sourceGeneration === status.sourceGeneration;
      if (identical) {
        this.#settle();
        return;
      }
    }
    this.#publish({ status, loading: false });
  }

  async refresh(): Promise<void> {
    if (this.#disposed) return;
    if (this.#refreshing) return this.#refreshing;
    this.#publish({ ...this.#state, loading: true, error: undefined });
    // A bootstrap that failed left this entry without a watch, and the entry is
    // shared — so the explicit refresh is also how every consumer of this
    // repository gets its observation back. One attempt at a time, or the
    // loser of the race would be a lease nothing releases.
    if (!this.#lease && this.#bootstrap === undefined) this.#start();
    const inFlight = (async () => {
      try {
        this.accept(await this.#client.status(this.#scope, this.#root, this.#abort.signal));
      } catch (cause) {
        if (this.#disposed || this.#abort.signal.aborted) return;
        this.#publish({ ...this.#state, loading: false, error: String(cause) });
      } finally {
        this.#refreshing = undefined;
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

  async push(repositoryId: string, expectedStatusGeneration: string): Promise<GitCommandResult> {
    const result = await this.#client.push(this.#scope, this.#root, repositoryId, expectedStatusGeneration);
    if (result.status) this.accept(result.status);
    return result;
  }

  #start(): void {
    const attempt = ++this.#attempts;
    this.#bootstrap = attempt;
    this.#stopEvents?.();
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
      if (this.#bootstrap === attempt) this.#bootstrap = undefined;
      if (this.#disposed
        || this.#attempts !== attempt
        || lease.rootToken !== this.#root.token
        || lease.connectionEpoch !== this.#scope.terminalEpoch) {
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
      if (this.#bootstrap === attempt) this.#bootstrap = undefined;
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

  /** Stops claiming to be loading, and clears any error, without a transition. */
  #settle(): void {
    if (!this.#state.loading && this.#state.error === undefined) return;
    this.#publish({ status: this.#state.status, loading: false, handle: this.#handle });
  }

  #publish(next: GitRepositoryState): void {
    this.#state = { ...next, handle: this.#handle };
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

  acquire(scope: FileWorkspaceScope, root: ActiveRoot): GitRepositoryLease {
    const key = gitScopeKey(scope, root);
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
      handle: owner.handle,
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
    const key = gitScopeKey(scope, root);
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

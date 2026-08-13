import { useCallback, useEffect, useRef, useState } from "react";
import { keyForScope, keyForTransferConnection, sameRoot } from "./api";
import { isTerminalTransferState, mergeCanonicalTransfer } from "../transfers/transferState";
import type {
  ActiveRoot,
  DirectoryListing,
  FileWorkspaceClient,
  FileWorkspaceScope,
  TransferStatus,
  WorkspaceEvent,
} from "./types";

interface WorkspaceFilesState {
  scopeKey: string;
  transferConnectionKey: string;
  root?: ActiveRoot;
  listings: ReadonlyMap<string, DirectoryListing>;
  expanded: ReadonlySet<string>;
  loading: ReadonlySet<string>;
  transfers: readonly TransferStatus[];
  error?: string;
}

const EMPTY = new Map<string, DirectoryListing>();

/**
 * How often the workspace root is re-resolved when nothing has changed. Every
 * event that *can* be pushed already re-resolves it immediately; this only
 * covers `cd` inside the current pane, which tmux does not announce.
 */
const ACTIVE_ROOT_POLL_MS = 2_000;

export function useWorkspaceFiles(client: FileWorkspaceClient, scope: FileWorkspaceScope | undefined) {
  const [state, setState] = useState<WorkspaceFilesState>({
    scopeKey: "",
    transferConnectionKey: "",
    listings: EMPTY,
    expanded: new Set(),
    loading: new Set(),
    transfers: [],
  });
  const stateRef = useRef(state);
  stateRef.current = state;
  const scopeEpoch = useRef(0);
  const rootProbeSerial = useRef(0);
  const directorySerial = useRef(new Map<string, number>());
  const scopeKey = scope ? keyForScope(scope) : "";
  const transferConnectionKey = scope ? keyForTransferConnection(scope) : "";
  const scopeRef = useRef(scope);
  scopeRef.current = scope;

  const loadDirectory = useCallback(async (path: string, force = false, append = false) => {
    const activeScope = scopeRef.current;
    if (!activeScope || keyForScope(activeScope) !== scopeKey) return;
    const root = stateRef.current.root;
    const previous = stateRef.current.listings.get(path);
    if (!root || (!force && !append && previous)) return;
    if (append && (!previous?.nextPageToken || previous.complete)) return;
    const epoch = scopeEpoch.current;
    const serial = (directorySerial.current.get(path) ?? 0) + 1;
    directorySerial.current.set(path, serial);
    setState((current) => ({ ...current, loading: new Set(current.loading).add(path), error: undefined }));
    try {
      const listing = await client.listDirectory(activeScope, root, path, append ? previous?.nextPageToken : undefined);
      if (epoch !== scopeEpoch.current || directorySerial.current.get(path) !== serial || keyForScope(activeScope) !== scopeKey) return;
      setState((current) => {
        if (!sameRoot(current.root, root) || listing.rootToken !== root.token) return current;
        const listings = new Map(current.listings);
        listings.set(path, append && previous ? {
          ...listing,
          entries: mergeEntries(previous.entries, listing.entries),
          overflowRecovery: previous.overflowRecovery || listing.overflowRecovery,
        } : listing);
        const loading = new Set(current.loading);
        loading.delete(path);
        return { ...current, listings, loading };
      });
    } catch (error) {
      setState((current) => {
        const loading = new Set(current.loading);
        loading.delete(path);
        return sameRoot(current.root, root) ? { ...current, loading, error: String(error) } : current;
      });
    }
  }, [client, scopeKey]);

  const applyEvent = useCallback((event: WorkspaceEvent) => {
    const current = stateRef.current;
    if (event.kind === "rootChanged") {
      // Root responses are also returned directly to the poller, where the
      // scope epoch and operation serial reject completions from the previous
      // pane. The broadcast has no caller epoch, so admitting it here would
      // reintroduce the stale cross-pane race that those barriers prevent.
      return;
    }
    if (event.kind === "transfer") {
      if (event.transfer.scopeKey !== transferConnectionKey || !scopeRef.current
        || keyForTransferConnection(scopeRef.current) !== transferConnectionKey) return;
      setState((value) => ({ ...value, transfers: upsertTransfer(value.transfers, event.transfer) }));
      return;
    }
    if (!current.root || event.rootToken !== current.root.token) return;
    if (event.kind === "directoryChanged") void loadDirectory(event.directory, true);
    else if (event.kind === "fileChanged" || event.kind === "fileDeleted") void loadDirectory(parentPath(event.path), true);
  }, [loadDirectory]);

  useEffect(() => {
    scopeEpoch.current += 1;
    if (!scope) {
      setState((current) => ({
        scopeKey: "", transferConnectionKey: "", listings: new Map(), expanded: new Set(), loading: new Set(),
        transfers: current.transfers.map(staleTransferOnScopeReplacement),
      }));
      return;
    }
    setState((current) => ({
      scopeKey,
      transferConnectionKey,
      listings: new Map(),
      expanded: new Set(),
      loading: new Set(),
      transfers: !current.transferConnectionKey || current.transferConnectionKey === transferConnectionKey
        ? current.transfers
        : current.transfers.map(staleTransferOnScopeReplacement),
    }));
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    const epoch = scopeEpoch.current;
    void client.subscribe(scope, applyEvent).then((stop) => {
      if (disposed) stop();
      else unsubscribe = stop;
    }).catch((error) => { if (!disposed) setState((value) => ({ ...value, error: String(error) })); });
    let resolving = false;
    const resolve = async () => {
      if (resolving) return;
      resolving = true;
      const probe = ++rootProbeSerial.current;
      try {
        const activeScope = scopeRef.current;
        if (!activeScope || keyForScope(activeScope) !== scopeKey) return;
        const root = await client.resolveActiveRoot(activeScope);
        if (disposed || epoch !== scopeEpoch.current || probe !== rootProbeSerial.current) return;
        setState((current) => sameRoot(current.root, root) ? current : {
          ...current,
          scopeKey,
          root,
          listings: new Map(),
          expanded: new Set([root.path]),
          loading: new Set(),
          error: undefined,
        });
      } catch (error) {
        if (!disposed && epoch === scopeEpoch.current && probe === rootProbeSerial.current) setState((current) => ({ ...current, error: String(error) }));
      } finally {
        resolving = false;
      }
    };
    void resolve();
    // A backstop, not the primary path. Pane and window changes already rebuild
    // this scope and re-resolve immediately, so the only thing left for a timer
    // to catch is the user running `cd` inside the pane they are already in —
    // for which tmux emits no notification at all, so nothing can push it.
    // At 350 ms this was a host round trip three times a second forever, which
    // over SSH is three round trips a second on an idle connection.
    const poll = window.setInterval(() => { void resolve(); }, ACTIVE_ROOT_POLL_MS);
    return () => {
      disposed = true;
      scopeEpoch.current += 1;
      window.clearInterval(poll);
      unsubscribe?.();
    };
  }, [applyEvent, client, scopeKey]);

  useEffect(() => {
    if (state.root && !state.listings.has(state.root.path)) void loadDirectory(state.root.path);
  }, [loadDirectory, state.listings, state.root]);

  useEffect(() => {
    const activeScope = scopeRef.current;
    if (!activeScope || !state.root) return;
    let disposed = false;
    let releases: (() => void)[] = [];
    void Promise.allSettled([...state.expanded].map((directory) => client.acquireDirectoryWatch(activeScope, state.root!, directory))).then((results) => {
      const next = results.filter((result): result is PromiseFulfilledResult<Awaited<ReturnType<FileWorkspaceClient["acquireDirectoryWatch"]>>> => result.status === "fulfilled").map((result) => result.value);
      if (disposed) next.forEach((lease) => lease.release());
      else {
        releases = next.map((lease) => lease.release);
        setState((current) => {
          if (!sameRoot(current.root, state.root)) return current;
          const listings = new Map(current.listings);
          for (const lease of next) if (lease.snapshot.rootToken === state.root!.token) listings.set(lease.snapshot.directory, lease.snapshot);
          return { ...current, listings };
        });
        const failure = results.find((result): result is PromiseRejectedResult => result.status === "rejected");
        if (failure) setState((current) => sameRoot(current.root, state.root) ? { ...current, error: String(failure.reason) } : current);
      }
    });
    return () => { disposed = true; releases.forEach((release) => release()); };
  }, [client, scopeKey, state.expanded, state.root]);

  const toggleDirectory = useCallback((path: string) => {
    setState((current) => {
      const expanded = new Set(current.expanded);
      if (expanded.has(path)) expanded.delete(path);
      else expanded.add(path);
      return { ...current, expanded };
    });
    if (!stateRef.current.listings.has(path)) void loadDirectory(path);
  }, [loadDirectory]);

  const refresh = useCallback((directory?: string) => {
    const target = directory ?? stateRef.current.root?.path;
    if (target) void loadDirectory(target, true);
  }, [loadDirectory]);

  const recordTransfer = useCallback((transfer: TransferStatus) => {
    if (transfer.scopeKey !== transferConnectionKey) return;
    setState((current) => ({ ...current, transfers: upsertTransfer(current.transfers, transfer) }));
  }, [transferConnectionKey]);

  const loadMore = useCallback((directory: string) => { void loadDirectory(directory, false, true); }, [loadDirectory]);

  // Effects run after paint. Mask the prior pane synchronously on the render
  // where scopeKey changes so Explorer never flashes or acts on the old root.
  const visible = state.scopeKey === scopeKey ? state : {
    scopeKey,
    transferConnectionKey,
    listings: EMPTY,
    expanded: new Set<string>(),
    loading: new Set<string>(),
    transfers: state.transferConnectionKey === transferConnectionKey
      ? state.transfers
      : state.transfers.map(staleTransferOnScopeReplacement),
  };
  return { ...visible, toggleDirectory, refresh, recordTransfer, loadMore };
}

function staleTransferOnScopeReplacement(transfer: TransferStatus): TransferStatus {
  if (isTerminalTransferState(transfer.state)) return transfer;
  return {
    ...transfer,
    state: "failed",
    outcome: transfer.state === "verifying" ? "unknown" : "notPublished",
    failureKind: "staleScope",
    error: "Transfer scope was replaced before its authoritative terminal event arrived.",
  };
}

function upsertTransfer(transfers: readonly TransferStatus[], next: TransferStatus): TransferStatus[] {
  const index = transfers.findIndex((item) => item.id === next.id);
  if (index < 0) return [next, ...transfers].slice(0, 100);
  const copy = [...transfers];
  copy[index] = mergeCanonicalTransfer(copy[index], next);
  return copy;
}

function mergeEntries(previous: readonly DirectoryListing["entries"][number][], next: readonly DirectoryListing["entries"][number][]) {
  const byPath = new Map(previous.map((entry) => [entry.path, entry]));
  for (const entry of next) byPath.set(entry.path, entry);
  return [...byPath.values()];
}

function parentPath(path: string): string {
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}

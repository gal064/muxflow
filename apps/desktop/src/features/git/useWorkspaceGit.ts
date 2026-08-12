import { useCallback, useEffect, useRef, useState } from "react";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitStatusSnapshot, GitWorkspaceClient, GitWorkspaceEvent } from "./types";

export interface WorkspaceGitState {
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
  refresh(): Promise<void>;
  accept(status: GitStatusSnapshot): void;
}

export function useWorkspaceGit(client: GitWorkspaceClient, scope: FileWorkspaceScope | undefined, root: ActiveRoot | undefined): WorkspaceGitState {
  const [status, setStatus] = useState<GitStatusSnapshot>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const serial = useRef(0);
  const abort = useRef<AbortController | undefined>(undefined);
  const identity = root && scope ? `${scope.clientId}\0${scope.serverIdentity}\0${scope.terminalEpoch}\0${root.token}\0${root.path}` : "";

  const accept = useCallback((next: GitStatusSnapshot) => {
    setStatus((current) => {
      if (current?.repository.id === next.repository.id && BigInt(next.generation) < BigInt(current.generation)) return current;
      return next;
    });
    setError(undefined);
  }, []);

  const refresh = useCallback(async () => {
    if (!scope || !root?.gitWorktree) return;
    const current = ++serial.current;
    abort.current?.abort();
    const controller = new AbortController();
    abort.current = controller;
    setLoading(true);
    try {
      const next = await client.status(scope, root, controller.signal);
      if (current !== serial.current || controller.signal.aborted) return;
      accept(next);
    } catch (cause) {
      if (controller.signal.aborted || current !== serial.current) return;
      setError(String(cause));
    } finally {
      if (current === serial.current) setLoading(false);
    }
  }, [accept, client, identity]);

  useEffect(() => {
    serial.current += 1;
    abort.current?.abort();
    setStatus(undefined);
    setError(undefined);
    setLoading(Boolean(scope && root?.gitWorktree));
    if (!scope || !root?.gitWorktree) return;
    let disposed = false;
    let release: (() => void) | undefined;
    const rootToken = root.token;
    const connectionEpoch = scope.terminalEpoch;
    let activeWatchId: string | undefined;
    const pendingEvents: GitWorkspaceEvent[] = [];
    const watchAbort = new AbortController();
    const stop = client.subscribe((event) => {
      if (disposed || event.rootToken !== rootToken) return;
      if (!activeWatchId) {
        pendingEvents.push(event);
        if (pendingEvents.length > 64) pendingEvents.shift();
        return;
      }
      if (event.watchId !== activeWatchId) return;
      if (event.kind === "error") setError(event.error);
      else accept(event.status);
    });
    void client.watch(scope, root, watchAbort.signal).then((lease) => {
      if (disposed || lease.rootToken !== rootToken || lease.connectionEpoch !== connectionEpoch) lease.release();
      else {
        release = lease.release;
        activeWatchId = lease.watchId;
        accept(lease.status);
        for (const event of pendingEvents) {
          if (event.watchId !== activeWatchId) continue;
          if (event.kind === "error") setError(event.error);
          else accept(event.status);
        }
        pendingEvents.length = 0;
        setLoading(false);
      }
    }).catch((cause) => {
      if (!disposed) {
        setError(String(cause));
        setLoading(false);
      }
    });
    return () => {
      disposed = true;
      serial.current += 1;
      abort.current?.abort();
      watchAbort.abort();
      stop();
      release?.();
    };
  }, [accept, client, identity]);

  return { status, loading, error, refresh, accept };
}

import { DiffEditor } from "@monaco-editor/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import { SurfaceError } from "../../ui/SurfaceError";
import type { GitCommandResult, GitDiff, GitMutationKind, GitMutationRequest, GitStatusSnapshot, GitWorkspaceClient, GitWorkspaceEvent } from "./types";
import { ADE_MONACO_THEME } from "../files/monaco";

interface Props {
  tab: AppOwnedTab;
  scope?: FileWorkspaceScope;
  activeRoot?: ActiveRoot;
  client: GitWorkspaceClient;
  canWrite: boolean;
  onMessage(message: string): void;
  onStatus(status: GitStatusSnapshot): void;
}

type PendingDiscard = { kind: "discardFile" | "discardHunk"; hunkIndex?: number; diff: GitDiff; status: GitStatusSnapshot; rootToken: string; connectionEpoch: number };

export function GitDiffSurface(props: Props) {
  const [diff, setDiff] = useState<GitDiff>();
  const [status, setStatus] = useState<GitStatusSnapshot>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  const serial = useRef(0);
  const abort = useRef<AbortController | undefined>(undefined);
  const loadedGeneration = useRef<string | undefined>(undefined);
  const watchedGeneration = useRef<string | undefined>(undefined);
  const root = useMemo<ActiveRoot | undefined>(() => props.tab.rootPath && props.tab.rootToken ? {
    path: props.tab.rootPath, cwd: props.tab.rootPath, token: props.tab.rootToken, paneId: props.scope?.paneId ?? "",
    gitWorktree: true, revision: "0",
  } : props.activeRoot, [props.activeRoot, props.scope?.paneId, props.tab.rootPath, props.tab.rootToken]);
  const repositoryId = props.tab.gitRepositoryId;
  const pathIdentity = props.tab.gitPath;
  const originalPathIdentity = props.tab.gitOriginalPath;
  const target = props.tab.gitTarget;

  const load = useCallback(async (clearStale = false) => {
    if (!props.scope || !root || !repositoryId || !pathIdentity || !target) return;
    const current = ++serial.current;
    abort.current?.abort();
    const controller = new AbortController();
    abort.current = controller;
    setLoading(true);
    if (clearStale) {
      setDiff(undefined);
      setStatus(undefined);
      setError(undefined);
    }
    try {
      const nextStatus = await props.client.status(props.scope, root, controller.signal);
      if (nextStatus.repository.id !== repositoryId) throw new Error("This diff belongs to a different repository. Return to its workspace or close the tab.");
      const nextEntry = nextStatus.entries.find((entry) => entry.path === pathIdentity);
      if (!nextEntry || (target === "staged" ? nextEntry.indexKind === "none" : nextEntry.worktreeKind === "none")) {
        if (current !== serial.current || controller.signal.aborted) return;
        loadedGeneration.current = nextStatus.generation;
        setStatus(nextStatus);
        setDiff(undefined);
        setError(undefined);
        props.onStatus(nextStatus);
        return;
      }
      const nextDiff = await props.client.diff(props.scope, root, repositoryId, pathIdentity, originalPathIdentity, target, nextStatus.generation, controller.signal);
      if (current !== serial.current || controller.signal.aborted) return;
      if (watchedGeneration.current && BigInt(watchedGeneration.current) > BigInt(nextStatus.generation)) {
        void load(clearStale);
        return;
      }
      if (!watchedGeneration.current || BigInt(nextStatus.generation) > BigInt(watchedGeneration.current)) watchedGeneration.current = nextStatus.generation;
      loadedGeneration.current = nextStatus.generation;
      setStatus(nextStatus);
      setDiff(nextDiff);
      setError(undefined);
      props.onStatus(nextStatus);
    } catch (cause) {
      if (controller.signal.aborted || current !== serial.current) return;
      setError(String(cause));
    } finally {
      if (current === serial.current) setLoading(false);
    }
  }, [props.client, props.scope?.clientId, props.scope?.terminalEpoch, repositoryId, pathIdentity, originalPathIdentity, root?.token, root?.path, target]);

  useEffect(() => {
    void load();
    return () => { serial.current += 1; abort.current?.abort(); loadedGeneration.current = undefined; watchedGeneration.current = undefined; };
  }, [load]);

  useEffect(() => {
    if (!props.scope || !root || !repositoryId) return;
    let disposed = false;
    let activeWatchId: string | undefined;
    const pendingEvents: GitWorkspaceEvent[] = [];
    let release: (() => void) | undefined;
    const watchAbort = new AbortController();
    const rootToken = root.token;
    const connectionEpoch = props.scope.terminalEpoch;
    const stop = props.client.subscribe((event) => {
      if (disposed || event.rootToken !== rootToken) return;
      if (!activeWatchId) {
        pendingEvents.push(event);
        if (pendingEvents.length > 64) pendingEvents.shift();
        return;
      }
      if (event.watchId !== activeWatchId) return;
      if (event.kind === "error") {
        setError(event.error);
        return;
      }
      if (event.status.repository.id !== repositoryId) return;
      watchedGeneration.current = event.status.generation;
      if (loadedGeneration.current !== event.status.generation) void load();
    });
    void props.client.watch(props.scope, root, watchAbort.signal).then((lease) => {
      if (disposed || lease.rootToken !== rootToken || lease.connectionEpoch !== connectionEpoch || lease.status.repository.id !== repositoryId) {
        lease.release();
        if (!disposed) setError("The saved Git tab no longer belongs to this repository.");
        return;
      }
      activeWatchId = lease.watchId;
      release = lease.release;
      watchedGeneration.current = lease.status.generation;
      let shouldReload = Boolean(loadedGeneration.current && loadedGeneration.current !== lease.status.generation);
      for (const event of pendingEvents) {
        if (event.watchId !== activeWatchId) continue;
        if (event.kind === "error") setError(event.error);
        else if (event.status.repository.id === repositoryId && BigInt(event.status.generation) >= BigInt(watchedGeneration.current)) {
          watchedGeneration.current = event.status.generation;
          if (loadedGeneration.current !== event.status.generation) shouldReload = true;
        }
      }
      pendingEvents.length = 0;
      if (shouldReload) void load();
    }).catch((cause) => { if (!disposed) setError(String(cause)); });
    return () => {
      disposed = true;
      watchAbort.abort();
      stop();
      release?.();
    };
  }, [props.client, props.scope?.clientId, props.scope?.serverIdentity, props.scope?.terminalEpoch, root?.path, root?.token, repositoryId, load]);

  const mutate = async (kind: GitMutationKind, hunkIndex?: number, confirmation?: string) => {
    if (!props.scope || !root || !status?.authoritative || !diff || !repositoryId || !props.canWrite) return;
    const request: GitMutationRequest = {
      kind, path: diff.path, ...(diff.originalPath ? { originalPath: diff.originalPath } : {}), target: diff.target,
      expectedStatusGeneration: status.generation, expectedSourceGeneration: diff.sourceGeneration,
      ...(hunkIndex !== undefined ? { hunkIndex } : {}), ...(confirmation ? { confirmationToken: confirmation } : {}),
    };
    setBusy(true);
    try {
      const result = await props.client.mutate(props.scope, root, repositoryId, request);
      reportResult(result, props.onMessage);
      if (result.status) props.onStatus(result.status);
      await load(true);
    } catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  };

  const confirmDiscard = async (pending: PendingDiscard) => {
    if (!props.scope || !root || !repositoryId || !pending.status.authoritative) return;
    if (root.token !== pending.rootToken || props.scope.terminalEpoch !== pending.connectionEpoch || repositoryId !== pending.status.repository.id) {
      setError("Discard was cancelled because the repository connection changed.");
      return;
    }
    const request: GitMutationRequest = {
      kind: pending.kind, path: pending.diff.path, ...(pending.diff.originalPath ? { originalPath: pending.diff.originalPath } : {}), target: pending.diff.target,
      expectedStatusGeneration: pending.status.generation, expectedSourceGeneration: pending.diff.sourceGeneration,
      ...(pending.hunkIndex !== undefined ? { hunkIndex: pending.hunkIndex } : {}),
    };
    setBusy(true);
    try {
      const token = await props.client.prepareDiscard(props.scope, root, repositoryId, request);
      const result = await props.client.mutate(props.scope, root, repositoryId, { ...request, confirmationToken: token });
      reportResult(result, props.onMessage);
      if (result.status) props.onStatus(result.status);
      await load(true);
    } catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  };

  if (!props.scope || !root) return <GitDiffEmpty title={props.tab.title} detail="Reconnect to reopen this Git diff." />;
  if (!repositoryId || !pathIdentity || !target) return <GitDiffEmpty title={props.tab.title} detail="This saved Git tab is missing its repository identity." />;
  if (loading && !diff) return <GitDiffEmpty title={props.tab.title} detail="Loading Git diff…" />;
  if (error && !diff) return <GitDiffEmpty title={props.tab.title} detail={error} retry={() => void load()} />;
  if (!diff || !status) return <GitDiffEmpty title={props.tab.title} detail={`This file no longer has ${target} changes.`} retry={() => void load()} />;

  const text = decodeTextDiff(diff);
  const currentEntry = status.entries.find((entry) => entry.path === diff.path);
  const mutationBlock = currentEntry?.submodule ? "Submodule pointer changes are read-only in v1." : currentEntry?.conflicted ? "Resolve conflicts in the terminal before using Git actions." : undefined;
  const pathChange = currentEntry && [currentEntry.indexKind, currentEntry.worktreeKind].some((kind) => kind === "renamed" || kind === "copied");
  const canMutate = props.canWrite && status.authoritative && !mutationBlock;
  const hunkActions = !diff.binary && !diff.tooLarge && !mutationBlock && !pathChange;
  return <section className="git-diff-surface" role="tabpanel" aria-label={`${diff.target} diff ${diff.displayPath}`}>
    <header className="editor-toolbar git-diff-toolbar">
      <span className={`git-target ${diff.target}`}>{diff.target}</span>
      <code title={diff.displayPath}>{diff.displayPath}</code>
      <button disabled={busy} onClick={() => void load(true)} type="button">Refresh</button>
      {diff.target === "unstaged" && <button disabled={busy || !canMutate} onClick={() => void mutate("stageFile")} type="button">Stage file</button>}
      {diff.target === "staged" && <button disabled={busy || !canMutate} onClick={() => void mutate("unstageFile")} type="button">Unstage file</button>}
      <button className="danger" disabled={busy || !canMutate} onClick={() => setPendingDiscard({ kind: "discardFile", diff, status, rootToken: root.token, connectionEpoch: props.scope!.terminalEpoch })} type="button">Discard file…</button>
    </header>
    <div className="git-diff-errors">
      {mutationBlock && <div className="git-diff-error" role="note">{mutationBlock}</div>}
      {error && <SurfaceError className="git-diff-error" detail={error} />}
    </div>
    <div className="git-diff-content">
      {diff.binary || !text ? <GitDiffEmpty title={diff.displayPath} detail={diff.binary ? "Binary changes cannot be displayed or edited as text." : "This diff contains non-UTF-8 content and is shown safely as binary."} />
        : diff.tooLarge ? <GitDiffEmpty title={diff.displayPath} detail="This diff is too large for the editor. File-level Git actions remain available." />
          : <DiffEditor
            keepCurrentModifiedModel
            keepCurrentOriginalModel
            language={languageForPath(diff.displayPath)}
            modified={text.modified}
            modifiedModelPath={modelUri(props.tab, "modified")}
            options={{ automaticLayout: true, enableSplitViewResizing: true, minimap: { enabled: false }, originalEditable: false, readOnly: true, renderSideBySide: true, scrollBeyondLastLine: false }}
            original={text.original}
            originalModelPath={modelUri(props.tab, "original")}
            theme={ADE_MONACO_THEME}
          />}
    </div>
    {hunkActions && diff.hunkCount > 0 && <aside className="git-hunk-actions" aria-label="Complete hunk actions">
      {Array.from({ length: diff.hunkCount }, (_, hunkIndex) => <div key={hunkIndex}>
        <span>Hunk {hunkIndex + 1}</span>
        {diff.target === "unstaged" ? <button disabled={busy || !canMutate} onClick={() => void mutate("stageHunk", hunkIndex)} type="button">Stage</button>
          : <button disabled={busy || !canMutate} onClick={() => void mutate("unstageHunk", hunkIndex)} type="button">Unstage</button>}
        <button className="danger" disabled={busy || !canMutate} onClick={() => setPendingDiscard({ kind: "discardHunk", hunkIndex, diff, status, rootToken: root.token, connectionEpoch: props.scope!.terminalEpoch })} type="button">Discard…</button>
      </div>)}
    </aside>}
    {pendingDiscard && <ConfirmationDialog
      confirmLabel="Discard"
      detail={`Discard ${pendingDiscard.hunkIndex === undefined ? "all changes" : `hunk ${pendingDiscard.hunkIndex + 1}`} in ${pendingDiscard.diff.displayPath}? This cannot be undone by the app.`}
      onCancel={() => setPendingDiscard(undefined)}
      onConfirm={() => { const captured = pendingDiscard; setPendingDiscard(undefined); void confirmDiscard(captured); }}
      title={pendingDiscard.hunkIndex === undefined ? "Discard file changes?" : "Discard complete hunk?"}
    />}
  </section>;
}

function decodeTextDiff(diff: GitDiff): { original: string; modified: string } | undefined {
  const decoder = new TextDecoder("utf-8", { fatal: true });
  try {
    return { original: diff.oldMissing ? "" : decoder.decode(diff.oldContent ?? new Uint8Array()), modified: diff.newMissing ? "" : decoder.decode(diff.newContent ?? new Uint8Array()) };
  } catch { return undefined; }
}

function GitDiffEmpty({ title, detail, retry }: { title: string; detail: string; retry?: () => void }) {
  return <div className="git-diff-empty"><div className="binary-icon" aria-hidden="true">±</div><h1>{title}</h1><p>{detail}</p>{retry && <button onClick={retry} type="button">Retry</button>}</div>;
}

function modelUri(tab: AppOwnedTab, side: string): string { return `tmux-ide-git://${encodeURIComponent(tab.hostProfileId)}/${encodeURIComponent(tab.serverIdentity)}/${encodeURIComponent(tab.id)}/${side}`; }
function languageForPath(path: string): string { const extension = path.split(".").at(-1)?.toLowerCase(); return ({ ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", rs: "rust", py: "python", md: "markdown", json: "json", css: "css", html: "html", sh: "shell", yml: "yaml", yaml: "yaml", toml: "ini" } as Record<string, string>)[extension ?? ""] ?? "plaintext"; }
function reportResult(result: GitCommandResult, report: (message: string) => void) {
  const command = [result.stdout.trim(), result.stderr.trim(), result.error].filter(Boolean).join(" · ") || (result.outcome === "applied" ? "Git change applied." : result.outcome === "partialOrUnknown" ? "Git outcome is partial or unknown; inspect the repository before retrying." : `Git failed with exit code ${result.exitCode}.`);
  report(result.refreshFailed ? `${command} ${result.statusOmitted ? "Post-command status was omitted to keep the connection responsive" : "Status refresh failed"}: ${result.refreshError}` : command);
}

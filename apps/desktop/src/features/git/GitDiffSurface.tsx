import { DiffEditor } from "@monaco-editor/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import { SurfaceError } from "../../ui/SurfaceError";
import { attachEditorLayout } from "../files/editorLayout";
import type { GitCommandResult, GitDiff, GitMutationKind, GitMutationRequest, GitStatusSnapshot } from "./types";
import { gitScopeKey, type GitRepositoryHandle, type GitRepositoryStore } from "./repositoryStore";
import { ADE_MONACO_THEME } from "../files/monaco";
import { recordPerfMilestone } from "../../perf/probe";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

interface Props {
  tab: AppOwnedTab;
  scope?: FileWorkspaceScope;
  activeRoot?: ActiveRoot;
  repositories: GitRepositoryStore;
  canWrite: boolean;
  onMessage(message: string): void;
}

type PendingDiscard = { kind: "discardFile" | "discardHunk"; hunkIndex?: number; diff: GitDiff; status: GitStatusSnapshot; rootToken: string; connectionEpoch: number };

export function GitDiffSurface(props: Props) {
  const detachLayout = useRef<(() => void) | undefined>(undefined);
  const editorSurfaceSequence = useRef(0);
  const mountedEditorSurface = useRef<number | undefined>(undefined);
  const readyEditorSurface = useRef<number | undefined>(undefined);
  const bindEditorHost = useCallback((node: HTMLDivElement | null) => {
    if (node) {
      mountedEditorSurface.current ??= ++editorSurfaceSequence.current;
    } else {
      mountedEditorSurface.current = undefined;
      readyEditorSurface.current = undefined;
    }
  }, []);
  useEffect(() => () => {
    detachLayout.current?.();
    detachLayout.current = undefined;
    mountedEditorSurface.current = undefined;
    readyEditorSurface.current = undefined;
  }, []);
  const [diff, setDiff] = useState<GitDiff>();
  const [status, setStatus] = useState<GitStatusSnapshot>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  // The shared observation's error, held separately so that clearing it does
  // not also clear an error this surface raised itself.
  const [sharedError, setSharedError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  const serial = useRef(0);
  const committedLoadSerial = useRef(0);
  const requestedGeneration = useRef<string | undefined>(undefined);
  const abort = useRef<AbortController | undefined>(undefined);
  // A command owns its own reload. While one is running the shared observation
  // will publish the command's authoritative status, and reacting to that would
  // start the same reload a second time.
  const commanding = useRef(false);
  const pendingDiffPaint = useRef<PaintTicket | undefined>(undefined);
  const repository = useRef<GitRepositoryHandle | undefined>(undefined);
  const root = useMemo<ActiveRoot | undefined>(() => props.tab.rootPath && props.tab.rootToken ? {
    path: props.tab.rootPath, cwd: props.tab.rootPath, token: props.tab.rootToken, paneId: props.scope?.paneId ?? "",
    gitWorktree: true, revision: "0",
  } : props.activeRoot, [props.activeRoot, props.scope?.paneId, props.tab.rootPath, props.tab.rootToken]);
  const repositoryId = props.tab.gitRepositoryId;
  const pathIdentity = props.tab.gitPath;
  const originalPathIdentity = props.tab.gitOriginalPath;
  const target = props.tab.gitTarget;
  // Memoized on the identity that keys the acquisition effect, so the effect
  // and the scope it acquires cannot describe different repositories.
  const boundScope = useMemo(
    () => (props.scope && root ? { scope: props.scope, root } : undefined),
    [props.scope, root],
  );
  const scopeIdentity = boundScope ? gitScopeKey(boundScope.scope, boundScope.root) : "";
  const decodedText = useMemo(() => diff ? decodeTextDiff(diff) : undefined, [diff]);
  const diffUsesEditor = Boolean(diff && !diff.binary && !diff.tooLarge && decodedText);
  useEffect(() => {
    if (diffUsesEditor) recordPerfMilestone("editor.monacoRequest");
  }, [diffUsesEditor]);

  /**
   * One round trip. The response states the authoritative status it was read
   * against, so this surface never asks for status first, and a second tab on
   * the same repository reuses the shared observation instead of starting its
   * own discovery and status pipeline.
   */
  const load = useCallback(async (clearStale = false) => {
    const handle = repository.current;
    if (!handle || !repositoryId || !pathIdentity || !target) return;
    const current = ++serial.current;
    // Claimed before awaiting so a watch event describing the same status
    // cannot start a second read of the same thing.
    requestedGeneration.current = handle.state().status?.generation;
    // A superseded load stops its own request, including the bulk body stream
    // it may already have started. Peers waiting on the same diff do not.
    abort.current?.abort();
    const controller = new AbortController();
    abort.current = controller;
    setLoading(true);
    pendingDiffPaint.current?.abandon();
    pendingDiffPaint.current = undefined;
    const paint = createPaintTicket(["workflow.git.diffPaint"], current);
    if (clearStale) {
      setDiff(undefined);
      setStatus(undefined);
      setError(undefined);
    }
    try {
      // The shared observation already knows whether this file still has the
      // change this tab is showing. When it does not, there is no diff to ask
      // for at all, which is what keeps a mutation to one request.
      const shared = handle.state().status;
      if (shared && shared.repository.id !== repositoryId) {
        // A saved tab whose root now resolves to a different repository has no
        // diff to ask for. Failing here keeps that explicit instead of sending
        // a request that can only be refused.
        throw new Error("This diff belongs to a different repository. Return to its workspace or close the tab.");
      }
      if (shared && !entryStillChanged(shared, pathIdentity, target)) {
        paint.abandon();
        setStatus(shared);
        setDiff(undefined);
        setError(undefined);
        return;
      }
      const result = await handle.diff({
        repositoryId,
        path: pathIdentity,
        ...(originalPathIdentity ? { originalPath: originalPathIdentity } : {}),
        target,
      }, controller.signal);
      if (current !== serial.current) {
        paint.abandon();
        return;
      }
      if (result.status.repository.id !== repositoryId) {
        throw new Error("This diff belongs to a different repository. Return to its workspace or close the tab.");
      }
      // The response states which status it was read against, which may be
      // newer than the one this load set out from. Claiming it stops the
      // subscriber from discarding a perfectly authoritative diff as stale.
      requestedGeneration.current = result.status.generation;
      setStatus(result.status);
      if (!entryStillChanged(result.status, pathIdentity, target)) {
        paint.abandon();
        setDiff(undefined);
        setError(undefined);
        return;
      }
      setDiff(result.diff);
      pendingDiffPaint.current = paint;
      setError(undefined);
    } catch (cause) {
      paint.abandon();
      // The attempted generation is deliberately retained: a failure that the
      // repository state has not moved past must not be retried on every
      // subsequent watch publication. Retry is the user's, through the button.
      if (current !== serial.current || isAbort(cause)) return;
      setError(String(cause));
    } finally {
      if (current === serial.current) setLoading(false);
    }
  }, [originalPathIdentity, pathIdentity, repositoryId, target]);
  // The subscription outlives any one `load`: its lifetime is the shared
  // observation's, and the diff this tab wants can change without the
  // repository changing. Reading the current `load` through a ref is what keeps
  // those two lifetimes independent without ever invoking a stale one.
  const currentLoad = useRef(load);
  currentLoad.current = load;

  /**
   * What the Refresh and Retry controls mean.
   *
   * `load` deliberately answers from the shared observation when that already
   * says this file has no such change. That is right for an automatic reload
   * and wrong for a person asking again, so an explicit refresh re-reads the
   * repository first and then decides.
   */
  const refreshFromHost = useCallback(async () => {
    const before = requestedGeneration.current;
    await repository.current?.refresh();
    // A refresh that moved the repository has already started the reload
    // through the subscription; forcing a second one here would be the same
    // request twice. Only a refresh that changed nothing still owes a read.
    if (repository.current?.state().status?.generation === before) await load(true);
  }, [load]);

  // The shared repository observation. Acquiring it is what makes a matching
  // diff tab free: it joins the sidebar's watch rather than opening its own.
  const repositories = props.repositories;
  useEffect(() => {
    if (!scopeIdentity || !boundScope || !repositoryId) return;
    const lease = repositories.acquire(boundScope.scope, boundScope.root);
    const acquired = lease.handle;
    repository.current = acquired;
    // Loading is driven by the shared status, never ahead of it: reading a diff
    // before the repository is observed would fetch against an unknown state
    // and then immediately fetch again.
    const loadWhenStatusMoves = () => {
      const next = acquired.state();
      setSharedError(next.error);
      // The observation has answered, even if the answer is that it failed.
      // Nothing else clears this: `load` is the only other place that does, and
      // it never runs without a status.
      if (!next.loading) setLoading(false);
      if (commanding.current || !next.status) return;
      if (next.status.generation === requestedGeneration.current) return;
      void currentLoad.current();
    };
    loadWhenStatusMoves();
    const stop = acquired.subscribe(loadWhenStatusMoves);
    return () => {
      serial.current += 1;
      committedLoadSerial.current = 0;
      pendingDiffPaint.current?.abandon();
      pendingDiffPaint.current = undefined;
      requestedGeneration.current = undefined;
      abort.current?.abort();
      abort.current = undefined;
      stop();
      lease.release();
      repository.current = undefined;
    };
    // `scopeIdentity` is the complete key of `boundScope`, and `load` is
    // reached only through `currentLoad`; neither belongs in this lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repositories, repositoryId, scopeIdentity]);

  useEffect(() => {
    if (loading || error || !diff) return;
    committedLoadSerial.current = serial.current;
    const paint = pendingDiffPaint.current;
    if (!paint) return;
    if (diffUsesEditor) {
      paint.expectSurface(
        mountedEditorSurface.current ?? editorSurfaceSequence.current + 1,
      );
      if (paint.surfaceGeneration !== mountedEditorSurface.current
        || readyEditorSurface.current !== mountedEditorSurface.current) return;
    }
    pendingDiffPaint.current = undefined;
    paint.afterPaint((ticket) => ticket.lifecycleGeneration === serial.current
      && ticket.lifecycleGeneration === committedLoadSerial.current
      && (!diffUsesEditor || ticket.surfaceGeneration === mountedEditorSurface.current),
    diffUsesEditor ? () => recordPerfMilestone("editor.paint") : undefined);
  }, [diff, diffUsesEditor, error, loading]);

  /**
   * One request and one post-command refresh. The result carries the
   * authoritative status, which the shared observation adopts; the only thing
   * still owed afterwards is this file's remaining diff, not a fresh
   * status-then-diff chain.
   */
  const applyCommand = async (
    run: () => Promise<GitCommandResult>,
  ) => {
    setBusy(true);
    commanding.current = true;
    try {
      const result = await run();
      reportResult(result, props.onMessage);
      // The shared observation already reconciled the command's authoritative
      // status, so the only thing still owed is this file's remaining diff.
      commanding.current = false;
      await load(true);
    } catch (cause) { setError(String(cause)); }
    finally {
      commanding.current = false;
      setBusy(false);
    }
  };

  const mutate = async (kind: GitMutationKind, hunkIndex?: number) => {
    const owner = repository.current;
    if (!owner || !status?.authoritative || !diff || !repositoryId || !props.canWrite) return;
    const request: GitMutationRequest = {
      kind, path: diff.path, ...(diff.originalPath ? { originalPath: diff.originalPath } : {}), target: diff.target,
      expectedStatusGeneration: status.generation, expectedSourceGeneration: diff.sourceGeneration,
      ...(hunkIndex !== undefined ? { hunkIndex } : {}),
    };
    await applyCommand(() => owner.mutate(repositoryId, request));
  };

  const confirmDiscard = async (pending: PendingDiscard) => {
    const scope = props.scope;
    const owner = repository.current;
    if (!owner || !scope || !root || !repositoryId || !pending.status.authoritative) return;
    if (root.token !== pending.rootToken || scope.terminalEpoch !== pending.connectionEpoch || repositoryId !== pending.status.repository.id) {
      setError("Discard was cancelled because the repository connection changed.");
      return;
    }
    const request: GitMutationRequest = {
      kind: pending.kind, path: pending.diff.path, ...(pending.diff.originalPath ? { originalPath: pending.diff.originalPath } : {}), target: pending.diff.target,
      expectedStatusGeneration: pending.status.generation, expectedSourceGeneration: pending.diff.sourceGeneration,
      ...(pending.hunkIndex !== undefined ? { hunkIndex: pending.hunkIndex } : {}),
    };
    await applyCommand(async () => {
      const token = await owner.prepareDiscard(repositoryId, request);
      return owner.mutate(repositoryId, { ...request, confirmationToken: token });
    });
  };

  if (!props.scope || !root) return <GitDiffEmpty title={props.tab.title} detail="Reconnect to reopen this Git diff." />;
  if (!repositoryId || !pathIdentity || !target) return <GitDiffEmpty title={props.tab.title} detail="This saved Git tab is missing its repository identity." />;
  const surfaceError = error ?? sharedError;
  // An error outranks a spinner: a surface still claiming to load while it
  // holds a failure is a surface with no way out of it.
  if (surfaceError && !diff) return <GitDiffEmpty title={props.tab.title} detail={surfaceError} retry={() => void refreshFromHost()} />;
  if (loading && !diff) return <GitDiffEmpty title={props.tab.title} detail="Loading Git diff…" retry={() => void refreshFromHost()} />;
  if (!diff || !status) return <GitDiffEmpty title={props.tab.title} detail={`This file no longer has ${target} changes.`} retry={() => void refreshFromHost()} />;

  const text = decodedText;
  const currentEntry = status.entries.find((entry) => entry.path === diff.path);
  const mutationBlock = currentEntry?.submodule ? "Submodule pointer changes are read-only in v1." : currentEntry?.conflicted ? "Resolve conflicts in the terminal before using Git actions." : undefined;
  const pathChange = currentEntry && [currentEntry.indexKind, currentEntry.worktreeKind].some((kind) => kind === "renamed" || kind === "copied");
  const canMutate = props.canWrite && status.authoritative && !mutationBlock;
  const hunkActions = !diff.binary && !diff.tooLarge && !mutationBlock && !pathChange;
  return <section className="git-diff-surface" role="tabpanel" aria-label={`${diff.target} diff ${diff.displayPath}`}>
    <header className="editor-toolbar git-diff-toolbar">
      <span className={`git-target ${diff.target}`}>{diff.target}</span>
      <code title={diff.displayPath}>{diff.displayPath}</code>
      <button disabled={busy} onClick={() => void refreshFromHost()} type="button">Refresh</button>
      {diff.target === "unstaged" && <button disabled={busy || !canMutate} onClick={() => void mutate("stageFile")} type="button">Stage file</button>}
      {diff.target === "staged" && <button disabled={busy || !canMutate} onClick={() => void mutate("unstageFile")} type="button">Unstage file</button>}
      <button className="danger" disabled={busy || !canMutate} onClick={() => setPendingDiscard({ kind: "discardFile", diff, status, rootToken: root.token, connectionEpoch: props.scope!.terminalEpoch })} type="button">Discard file…</button>
    </header>
    <div className="git-diff-errors">
      {mutationBlock && <div className="git-diff-error" role="note">{mutationBlock}</div>}
      {surfaceError && <SurfaceError className="git-diff-error" detail={surfaceError} />}
    </div>
    <div className="git-diff-content" ref={diffUsesEditor ? bindEditorHost : undefined}>
      {diff.binary || !text ? <GitDiffEmpty title={diff.displayPath} detail={diff.binary ? "Binary changes cannot be displayed or edited as text." : "This diff contains non-UTF-8 content and is shown safely as binary."} />
        : diff.tooLarge ? <GitDiffEmpty title={diff.displayPath} detail="This diff is too large for the editor. File-level Git actions remain available." />
          : <DiffEditor
            keepCurrentModifiedModel
            keepCurrentOriginalModel
            language={languageForPath(diff.displayPath)}
            modified={text.modified}
            modifiedModelPath={modelUri(props.tab, "modified")}
            onMount={(editor) => {
              const paint = pendingDiffPaint.current;
              const surface = mountedEditorSurface.current;
              readyEditorSurface.current = surface;
              if (paint && surface
                && paint.surfaceGeneration === surface
                && paint.lifecycleGeneration === committedLoadSerial.current) {
                pendingDiffPaint.current = undefined;
                paint.afterPaint((ticket) => ticket.lifecycleGeneration === serial.current
                  && ticket.lifecycleGeneration === committedLoadSerial.current
                  && ticket.surfaceGeneration === mountedEditorSurface.current,
                () => recordPerfMilestone("editor.paint"));
              }
              detachLayout.current?.(); detachLayout.current = attachEditorLayout(editor);
            }}
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
      destructive
      detail={`Discard ${pendingDiscard.hunkIndex === undefined ? "all changes" : `hunk ${pendingDiscard.hunkIndex + 1}`} in ${pendingDiscard.diff.displayPath}? This cannot be undone by the app.`}
      onCancel={() => setPendingDiscard(undefined)}
      onConfirm={() => { const captured = pendingDiscard; setPendingDiscard(undefined); void confirmDiscard(captured); }}
      title={pendingDiscard.hunkIndex === undefined ? "Discard file changes?" : "Discard complete hunk?"}
    />}
  </section>;
}

/** Whether the status still reports the change this tab is displaying. */
function entryStillChanged(status: GitStatusSnapshot, path: string, target: GitDiff["target"]): boolean {
  const entry = status.entries.find((candidate) => candidate.path === path);
  if (!entry) return false;
  return target === "staged" ? entry.indexKind !== "none" : entry.worktreeKind !== "none";
}

/** The one boundary that means "this was cancelled", not "this failed". */
function isAbort(cause: unknown): boolean {
  return cause instanceof DOMException && cause.name === "AbortError";
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

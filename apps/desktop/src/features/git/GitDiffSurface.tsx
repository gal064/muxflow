import { DiffEditor } from "@monaco-editor/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import { SurfaceError } from "../../ui/SurfaceError";
import { attachEditorLayout } from "../files/editorLayout";
import type { GitCommandResult, GitDiff, GitMutationKind, GitMutationRequest, GitStatusSnapshot, GitWorkspaceClient } from "./types";
import type { GitRepositoryHandle, GitRepositoryStore } from "./repositoryStore";
import { ADE_MONACO_THEME } from "../files/monaco";
import { recordPerfMilestone } from "../../perf/probe";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

interface Props {
  tab: AppOwnedTab;
  scope?: FileWorkspaceScope;
  activeRoot?: ActiveRoot;
  client: GitWorkspaceClient;
  repositories: GitRepositoryStore;
  canWrite: boolean;
  onMessage(message: string): void;
  onStatus(status: GitStatusSnapshot): void;
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
  const [busy, setBusy] = useState(false);
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  const serial = useRef(0);
  const committedLoadSerial = useRef(0);
  const loadedGeneration = useRef<string | undefined>(undefined);
  const requestedGeneration = useRef<string | undefined>(undefined);
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
  // Held in refs so acquiring the shared observation does not re-run whenever
  // an unrelated prop identity changes.
  const publishStatus = useRef(props.onStatus);
  publishStatus.current = props.onStatus;
  const boundScope = useRef<{ scope: FileWorkspaceScope; root: ActiveRoot } | undefined>(undefined);
  boundScope.current = props.scope && root ? { scope: props.scope, root } : undefined;
  const scopeIdentity = props.scope && root
    ? `${props.scope.clientId}\0${props.scope.serverIdentity}\0${props.scope.terminalEpoch}\0${root.token}\0${root.path}`
    : "";
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
        loadedGeneration.current = shared.generation;
        setStatus(shared);
        setDiff(undefined);
        setError(undefined);
        publishStatus.current(shared);
        return;
      }
      const result = await handle.diff({
        repositoryId,
        path: pathIdentity,
        ...(originalPathIdentity ? { originalPath: originalPathIdentity } : {}),
        target,
      });
      if (current !== serial.current) {
        paint.abandon();
        return;
      }
      if (result.status.repository.id !== repositoryId) {
        throw new Error("This diff belongs to a different repository. Return to its workspace or close the tab.");
      }
      loadedGeneration.current = result.status.generation;
      setStatus(result.status);
      if (!entryStillChanged(result.status, pathIdentity, target)) {
        paint.abandon();
        setDiff(undefined);
        setError(undefined);
        publishStatus.current(result.status);
        return;
      }
      setDiff(result.diff);
      pendingDiffPaint.current = paint;
      setError(undefined);
      publishStatus.current(result.status);
    } catch (cause) {
      paint.abandon();
      requestedGeneration.current = undefined;
      if (current !== serial.current || String(cause).includes("cancelled")) return;
      setError(String(cause));
    } finally {
      if (current === serial.current) setLoading(false);
    }
  }, [originalPathIdentity, pathIdentity, repositoryId, target]);

  // The shared repository observation. Acquiring it is what makes a matching
  // diff tab free: it joins the sidebar's watch rather than opening its own.
  const repositories = props.repositories;
  useEffect(() => {
    const bound = boundScope.current;
    if (!scopeIdentity || !bound || !repositoryId) return;
    const acquired = repositories.acquire(bound.scope, bound.root);
    repository.current = acquired;
    // Loading is driven by the shared status, never ahead of it: reading a diff
    // before the repository is observed would fetch against an unknown state
    // and then immediately fetch again.
    const loadWhenStatusMoves = () => {
      const next = acquired.state();
      if (next.error) setError(next.error);
      if (!next.status) return;
      if (next.status.generation === requestedGeneration.current) return;
      void load();
    };
    loadWhenStatusMoves();
    const stop = acquired.subscribe(loadWhenStatusMoves);
    return () => {
      serial.current += 1;
      committedLoadSerial.current = 0;
      pendingDiffPaint.current?.abandon();
      pendingDiffPaint.current = undefined;
      loadedGeneration.current = undefined;
      requestedGeneration.current = undefined;
      stop();
      acquired.release();
      repository.current = undefined;
    };
  }, [load, repositories, repositoryId, scopeIdentity]);

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
    try {
      const result = await run();
      reportResult(result, props.onMessage);
      if (result.status) {
        repository.current?.accept(result.status);
        publishStatus.current(result.status);
      }
      // Only the remaining diff is owed: the command already delivered the
      // authoritative status. Clearing first keeps a staged file from still
      // being shown as an unstaged change while that reload runs.
      await load(true);
    } catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  };

  const mutate = async (kind: GitMutationKind, hunkIndex?: number) => {
    const scope = props.scope;
    if (!scope || !root || !status?.authoritative || !diff || !repositoryId || !props.canWrite) return;
    const request: GitMutationRequest = {
      kind, path: diff.path, ...(diff.originalPath ? { originalPath: diff.originalPath } : {}), target: diff.target,
      expectedStatusGeneration: status.generation, expectedSourceGeneration: diff.sourceGeneration,
      ...(hunkIndex !== undefined ? { hunkIndex } : {}),
    };
    await applyCommand(() => props.client.mutate(scope, root, repositoryId, request));
  };

  const confirmDiscard = async (pending: PendingDiscard) => {
    const scope = props.scope;
    if (!scope || !root || !repositoryId || !pending.status.authoritative) return;
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
      const token = await props.client.prepareDiscard(scope, root, repositoryId, request);
      return props.client.mutate(scope, root, repositoryId, { ...request, confirmationToken: token });
    });
  };

  if (!props.scope || !root) return <GitDiffEmpty title={props.tab.title} detail="Reconnect to reopen this Git diff." />;
  if (!repositoryId || !pathIdentity || !target) return <GitDiffEmpty title={props.tab.title} detail="This saved Git tab is missing its repository identity." />;
  if (loading && !diff) return <GitDiffEmpty title={props.tab.title} detail="Loading Git diff…" />;
  if (error && !diff) return <GitDiffEmpty title={props.tab.title} detail={error} retry={() => void load()} />;
  if (!diff || !status) return <GitDiffEmpty title={props.tab.title} detail={`This file no longer has ${target} changes.`} retry={() => void load()} />;

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
      <button disabled={busy} onClick={() => void load(true)} type="button">Refresh</button>
      {diff.target === "unstaged" && <button disabled={busy || !canMutate} onClick={() => void mutate("stageFile")} type="button">Stage file</button>}
      {diff.target === "staged" && <button disabled={busy || !canMutate} onClick={() => void mutate("unstageFile")} type="button">Unstage file</button>}
      <button className="danger" disabled={busy || !canMutate} onClick={() => setPendingDiscard({ kind: "discardFile", diff, status, rootToken: root.token, connectionEpoch: props.scope!.terminalEpoch })} type="button">Discard file…</button>
    </header>
    <div className="git-diff-errors">
      {mutationBlock && <div className="git-diff-error" role="note">{mutationBlock}</div>}
      {error && <SurfaceError className="git-diff-error" detail={error} />}
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

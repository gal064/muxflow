import { DiffEditor } from "@monaco-editor/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import { SurfaceError } from "../../ui/SurfaceError";
import { attachEditorLayout } from "../files/editorLayout";
import type { GitCommandResult, GitDiff, GitMutationKind, GitMutationRequest, GitStatusSnapshot } from "./types";
import type { GitRepositoryStore } from "./repositoryStore";
import { useSharedGitDiff } from "./useSharedGitDiff";
import { ADE_MONACO_THEME } from "../files/monaco";
import { recordPerfMilestone } from "../../perf/probe";

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
  const [busy, setBusy] = useState(false);
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  const root = useMemo<ActiveRoot | undefined>(() => props.tab.rootPath && props.tab.rootToken ? {
    path: props.tab.rootPath, cwd: props.tab.rootPath, token: props.tab.rootToken, paneId: props.scope?.paneId ?? "",
    gitWorktree: true, revision: "0",
  } : props.activeRoot, [props.activeRoot, props.scope?.paneId, props.tab.rootPath, props.tab.rootToken]);
  const repositoryId = props.tab.gitRepositoryId;

  // Everything about when this diff is read, including the lease on the shared
  // repository observation. This component owns only what is drawn.
  const shared = useSharedGitDiff({
    repositories: props.repositories,
    scope: props.scope,
    root,
    repositoryId,
    path: props.tab.gitPath,
    originalPath: props.tab.gitOriginalPath,
    target: props.tab.gitTarget,
  });
  const { diff, status, paint } = shared;

  const decodedText = useMemo(() => diff ? decodeTextDiff(diff) : undefined, [diff]);
  const diffUsesEditor = Boolean(diff && !diff.binary && !diff.tooLarge && decodedText);
  useEffect(() => {
    if (diffUsesEditor) recordPerfMilestone("editor.monacoRequest");
  }, [diffUsesEditor]);

  const loading = shared.loading;
  const surfaceError = shared.error;
  useEffect(() => {
    if (loading || surfaceError || !diff) return;
    paint.committed.current = paint.lifecycle.current;
    const ticket = paint.pending.current;
    if (!ticket) return;
    if (diffUsesEditor) {
      ticket.expectSurface(
        mountedEditorSurface.current ?? editorSurfaceSequence.current + 1,
      );
      if (ticket.surfaceGeneration !== mountedEditorSurface.current
        || readyEditorSurface.current !== mountedEditorSurface.current) return;
    }
    paint.pending.current = undefined;
    ticket.afterPaint((held) => held.lifecycleGeneration === paint.lifecycle.current
      && held.lifecycleGeneration === paint.committed.current
      && (!diffUsesEditor || held.surfaceGeneration === mountedEditorSurface.current),
    diffUsesEditor ? () => recordPerfMilestone("editor.paint") : undefined);
  }, [diff, diffUsesEditor, surfaceError, loading, paint]);

  const applyCommand = async (run: () => Promise<GitCommandResult>) => {
    setBusy(true);
    try {
      const result = await shared.command(run);
      if (result) reportResult(result, props.onMessage);
    } finally { setBusy(false); }
  };

  const mutate = async (kind: GitMutationKind, hunkIndex?: number) => {
    const owner = shared.repository;
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
    const owner = shared.repository;
    if (!owner || !scope || !root || !repositoryId || !pending.status.authoritative) return;
    if (root.token !== pending.rootToken || scope.terminalEpoch !== pending.connectionEpoch || repositoryId !== pending.status.repository.id) {
      shared.fail("Discard was cancelled because the repository connection changed.");
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

  const refresh = () => void shared.refresh();
  if (!props.scope || !root) return <GitDiffEmpty title={props.tab.title} detail="Reconnect to reopen this Git diff." />;
  if (!repositoryId || !props.tab.gitPath || !props.tab.gitTarget) return <GitDiffEmpty title={props.tab.title} detail="This saved Git tab is missing its repository identity." />;
  // An error outranks a spinner: a surface still claiming to load while it
  // holds a failure is a surface with no way out of it.
  if (surfaceError && !diff) return <GitDiffEmpty title={props.tab.title} detail={surfaceError} retry={refresh} />;
  if (loading && !diff) return <GitDiffEmpty title={props.tab.title} detail="Loading Git diff…" retry={refresh} />;
  if (!diff || !status) return <GitDiffEmpty title={props.tab.title} detail={`This file no longer has ${props.tab.gitTarget} changes.`} retry={refresh} />;

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
      <button disabled={busy} onClick={refresh} type="button">Refresh</button>
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
              const ticket = paint.pending.current;
              const surface = mountedEditorSurface.current;
              readyEditorSurface.current = surface;
              if (ticket && surface
                && ticket.surfaceGeneration === surface
                && ticket.lifecycleGeneration === paint.committed.current) {
                paint.pending.current = undefined;
                ticket.afterPaint((held) => held.lifecycleGeneration === paint.lifecycle.current
                  && held.lifecycleGeneration === paint.committed.current
                  && held.surfaceGeneration === mountedEditorSurface.current,
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

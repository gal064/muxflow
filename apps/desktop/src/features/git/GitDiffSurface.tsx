import { lazy, Suspense, useEffect, useMemo, useState } from "react";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { AppOwnedTab } from "../shell/types";
import { SurfaceError } from "../../ui/SurfaceError";
import type { GitCommandResult, GitDiff, GitMutationKind, GitMutationRequest, GitStatusSnapshot } from "./types";
import type { GitRepositoryStore } from "./repositoryStore";
import { useSharedGitDiff } from "./useSharedGitDiff";
import { recordPerfMilestone } from "../../perf/probe";
import { useEditorPaint } from "../../perf/surfacePaint";

/**
 * The editor bundle, fetched when a text diff is actually going to be shown.
 *
 * Nothing above this line imports Monaco: the diff request starts on mount, and
 * a binary diff, an oversized diff, a saved tab with no repository and a file
 * that no longer differs never fetch the editor at all.
 */
const GitDiffEditor = lazy(() => import("./GitDiffEditor").then((module) => ({ default: module.GitDiffEditor })));

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

  const shown = useMemo(() => shownDiff(diff), [diff]);
  const diffUsesEditor = shown.kind === "editor";
  useEffect(() => {
    if (diffUsesEditor) recordPerfMilestone("editor.monacoRequest");
  }, [diffUsesEditor]);

  const loading = shared.loading;
  const surfaceError = shared.error;
  const editor = useEditorPaint(paint, !loading && !surfaceError && Boolean(diff), diffUsesEditor);

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
    <div className="git-diff-content" ref={diffUsesEditor ? editor.bindHost : undefined}>
      {shown.kind === "editor"
        ? <Suspense fallback={<p className="quiet-empty">Loading editor…</p>}>
          <GitDiffEditor
            modified={shown.text.modified}
            modifiedModelPath={modelUri(props.tab, "modified")}
            onReady={editor.onReady}
            original={shown.text.original}
            originalModelPath={modelUri(props.tab, "original")}
            path={diff.displayPath}
          />
        </Suspense>
        : shown.kind === "blocked" ? <GitDiffEmpty title={diff.displayPath} detail={shown.detail} /> : null}
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

/**
 * What the content area of a diff tab has to show, decided once.
 *
 * Whether an editor is used and what to say when it is not were two ladders
 * over the same three facts, so a fourth reason to refuse an editor would have
 * been added to one of them and silently explained by the other's last branch.
 */
type ShownDiff =
  /** Nothing to draw: the surface's own empty states have already said why. */
  | { kind: "absent" }
  /** Displayable, but not as text in an editor. */
  | { kind: "blocked"; detail: string }
  | { kind: "editor"; text: { original: string; modified: string } };

function shownDiff(diff: GitDiff | undefined): ShownDiff {
  if (!diff) return { kind: "absent" };
  if (diff.binary) return { kind: "blocked", detail: "Binary changes cannot be displayed or edited as text." };
  const text = decodeTextDiff(diff);
  if (!text) return { kind: "blocked", detail: "This diff contains non-UTF-8 content and is shown safely as binary." };
  if (diff.tooLarge) return { kind: "blocked", detail: "This diff is too large for the editor. File-level Git actions remain available." };
  return { kind: "editor", text };
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
function reportResult(result: GitCommandResult, report: (message: string) => void) {
  const command = [result.stdout.trim(), result.stderr.trim(), result.error].filter(Boolean).join(" · ") || (result.outcome === "applied" ? "Git change applied." : result.outcome === "partialOrUnknown" ? "Git outcome is partial or unknown; inspect the repository before retrying." : `Git failed with exit code ${result.exitCode}.`);
  report(result.refreshFailed ? `${command} ${result.statusOmitted ? "Post-command status was omitted to keep the connection responsive" : "Status refresh failed"}: ${result.refreshError}` : command);
}

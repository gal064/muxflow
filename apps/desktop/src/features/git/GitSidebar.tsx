import { useMemo, useRef, useState } from "react";
import { keyboardEventIsComposing } from "../../commands/registry";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitCommandResult, GitDiffTarget, GitMutationRequest, GitStatusEntry, GitStatusSnapshot, GitWorkspaceClient } from "./types";

interface Props {
  client: GitWorkspaceClient;
  scope?: FileWorkspaceScope;
  root?: ActiveRoot;
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
  disabled: boolean;
  onOpenDiff(entry: GitStatusEntry, target: GitDiffTarget): void;
  onRefresh(): void;
  onStatus(status: GitStatusSnapshot): void;
  onMessage(message: string): void;
}

type PendingDiscard = { entry: GitStatusEntry; target: GitDiffTarget; status: GitStatusSnapshot; rootToken: string; connectionEpoch: number };

export function GitSidebar(props: Props) {
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  const [busyPath, setBusyPath] = useState<string>();
  const [commitMessage, setCommitMessage] = useState("");
  const [commitOutput, setCommitOutput] = useState<GitCommandResult>();
  const [commitError, setCommitError] = useState<string>();
  const commitComposing = useRef(false);
  const groups = useMemo(() => groupEntries(props.status?.entries ?? []), [props.status]);
  const unavailable = props.disabled || !props.scope || !props.root || !props.status?.authoritative;

  const mutateFile = async (entry: GitStatusEntry, target: GitDiffTarget, kind: GitMutationRequest["kind"], confirmed = false, capturedStatus = props.status) => {
    if (!props.scope || !props.root || !capturedStatus || unavailable) return;
    const request: GitMutationRequest = {
      kind, path: entry.path, ...(entry.originalPath ? { originalPath: entry.originalPath } : {}), target,
      expectedStatusGeneration: capturedStatus.generation, expectedSourceGeneration: capturedStatus.sourceGeneration,
    };
    setBusyPath(entry.path);
    try {
      if (kind === "discardFile") {
        if (!confirmed) throw new Error("Discard was not confirmed.");
        request.confirmationToken = await props.client.prepareDiscard(props.scope, props.root, capturedStatus.repository.id, request);
      }
      const result = await props.client.mutate(props.scope, props.root, capturedStatus.repository.id, request);
      if (result.status) props.onStatus(result.status);
      props.onMessage(gitResultMessage(result, `${labelFor(kind)} ${entry.displayPath}`));
    } catch (cause) { props.onMessage(String(cause)); }
    finally { setBusyPath(undefined); }
  };

  const commit = async () => {
    setCommitError(undefined);
    setCommitOutput(undefined);
    if (!commitMessage.trim()) {
      setCommitError("Enter a commit message.");
      return;
    }
    if (!props.scope || !props.root || !props.status || unavailable) return;
    try {
      const result = await props.client.commit(props.scope, props.root, props.status.repository.id, props.status.generation, commitMessage);
      setCommitOutput(result);
      props.onMessage(gitResultMessage(result, result.outcome === "applied" ? "Commit created." : "Commit failed."));
      if (result.status) props.onStatus(result.status);
      if (result.outcome === "applied") setCommitMessage("");
      else setCommitError(result.outcome === "partialOrUnknown" ? "Commit outcome is uncertain; inspect HEAD before retrying." : "Git did not create a commit.");
    } catch (cause) { setCommitError(String(cause)); }
  };

  if (!props.root) return <GitEmpty title="Source Control" detail="Select a terminal pane to discover its repository." />;
  if (!props.root.gitWorktree) return <GitEmpty title="No repository" detail="The active pane is outside a Git worktree." />;
  if (props.loading && !props.status) return <GitEmpty title="Source Control" detail="Reading Git status…" />;
  if (props.error && !props.status) return <GitEmpty title="Git unavailable" detail={props.error} action={props.onRefresh} />;
  if (!props.status) return <GitEmpty title="Source Control" detail="Git status is unavailable." action={props.onRefresh} />;
  if (props.status.oversized) return <GitEmpty title="Repository status is too large" detail={`${props.status.error || "The host bounded this snapshot to keep the terminal connection responsive."} ${props.status.totalEntryCount ?? "Unknown"} entries were detected.`} action={props.onRefresh} />;

  const stagedCount = groups.staged.length;
  return <section className="git-sidebar" aria-label="Source Control">
    <header className="git-sidebar-header">
      <div><strong>{props.status.repository.headName || (props.status.repository.initial ? "Initial repository" : "Detached HEAD")}</strong><small title={props.status.repository.worktreeRoot}>{props.status.repository.worktreeRoot}</small></div>
      <button aria-label="Refresh Git status" disabled={props.loading} onClick={props.onRefresh} type="button">↻</button>
    </header>
    {props.error && <div className="git-error" role="alert">{props.error}</div>}
    {props.status.copyDetectionIncomplete && <div className="git-error" role="status">Copy detection was bounded for this large change set; some copies may appear as additions.</div>}
    {!props.status.authoritative && <div className="git-error" role="alert">Git status is resynchronizing. Mutations are disabled.</div>}
    <form className="git-commit" onSubmit={(event) => { event.preventDefault(); if (!commitComposing.current) void commit(); }}>
      <label htmlFor="git-commit-message">Commit message</label>
      <textarea disabled={unavailable} id="git-commit-message" onChange={(event) => setCommitMessage(event.target.value)} onCompositionEnd={() => { commitComposing.current = false; }} onCompositionStart={() => { commitComposing.current = true; }} placeholder="Message (Ctrl+Enter to commit)" onKeyDown={(event) => {
        if (!keyboardEventIsComposing(event.nativeEvent) && event.key === "Enter" && (event.ctrlKey || event.metaKey)) { event.preventDefault(); void commit(); }
      }} value={commitMessage} />
      <button className="primary" disabled={unavailable || stagedCount === 0} type="submit">Commit {stagedCount ? `${stagedCount} staged` : ""}</button>
      {commitError && <span className="git-error" role="alert">{commitError}</span>}
      {commitOutput && <pre aria-label="Git commit output" className={commitOutput.outcome === "applied" && !commitOutput.refreshFailed ? "git-output" : "git-output error"}>{commandDetails(commitOutput, "Commit created.")}</pre>}
    </form>
    <div className="git-status-groups">
      <GitGroup title="Merge Changes" entries={groups.conflicts} target="unstaged" disabled={unavailable} busyPath={busyPath} onOpen={props.onOpenDiff} />
      <GitGroup title="Staged Changes" entries={groups.staged} target="staged" disabled={unavailable} busyPath={busyPath} onOpen={props.onOpenDiff} onPrimary={(entry) => void mutateFile(entry, "staged", "unstageFile")} primaryLabel="Unstage" onDiscard={(entry) => setPendingDiscard({ entry, target: "staged", status: props.status!, rootToken: props.root!.token, connectionEpoch: props.scope!.terminalEpoch })} />
      <GitGroup title="Changes" entries={groups.unstaged} target="unstaged" disabled={unavailable} busyPath={busyPath} onOpen={props.onOpenDiff} onPrimary={(entry) => void mutateFile(entry, "unstaged", "stageFile")} primaryLabel="Stage" onDiscard={(entry) => setPendingDiscard({ entry, target: "unstaged", status: props.status!, rootToken: props.root!.token, connectionEpoch: props.scope!.terminalEpoch })} />
      <GitGroup title="Untracked" entries={groups.untracked} target="unstaged" disabled={unavailable} busyPath={busyPath} onOpen={props.onOpenDiff} onPrimary={(entry) => void mutateFile(entry, "unstaged", "stageFile")} primaryLabel="Stage" onDiscard={(entry) => setPendingDiscard({ entry, target: "unstaged", status: props.status!, rootToken: props.root!.token, connectionEpoch: props.scope!.terminalEpoch })} />
      <GitGroup title="Ignored" entries={groups.ignored} target="unstaged" disabled entriesOnly busyPath={busyPath} onOpen={props.onOpenDiff} />
      {props.status.entries.length === 0 && <div className="git-clean"><span>✓</span><strong>Working tree clean</strong></div>}
    </div>
    {pendingDiscard && <ConfirmationDialog
      confirmLabel="Discard"
      detail={`Discard ${pendingDiscard.entry.displayPath}? This cannot be undone by the app.`}
      onCancel={() => setPendingDiscard(undefined)}
      onConfirm={() => {
        const captured = pendingDiscard;
        setPendingDiscard(undefined);
        if (props.root?.token !== captured.rootToken || props.scope?.terminalEpoch !== captured.connectionEpoch || props.status?.repository.id !== captured.status.repository.id) {
          props.onMessage("Discard was cancelled because the repository connection changed.");
          return;
        }
        void mutateFile(captured.entry, captured.target, "discardFile", true, captured.status);
      }}
      title={pendingDiscard.entry.untracked ? "Delete untracked file?" : "Discard file changes?"}
    />}
  </section>;
}

function GitGroup(props: {
  title: string; entries: GitStatusEntry[]; target: GitDiffTarget; disabled: boolean; busyPath?: string; entriesOnly?: boolean;
  primaryLabel?: string; onPrimary?(entry: GitStatusEntry): void; onDiscard?(entry: GitStatusEntry): void;
  onOpen(entry: GitStatusEntry, target: GitDiffTarget): void;
}) {
  const [limit, setLimit] = useState(200);
  if (!props.entries.length) return null;
  const visible = props.entries.slice(0, limit);
  return <details open className="git-group"><summary><span>{props.title}</span><span className="git-count">{props.entries.length}</span></summary><ul>
    {visible.map((entry) => <li key={`${props.target}\0${entry.path}`} className={entry.conflicted ? "conflicted" : ""}>
      <button className="git-file" disabled={entry.ignored} onClick={() => props.onOpen(entry, props.target)} title={entry.displayPath} type="button">
        <span>{baseName(entry.displayPath)}</span><small>{parentName(entry.displayPath)}</small>
        <abbr title={statusTitle(entry, props.target)}>{statusCode(entry, props.target)}</abbr>
      </button>
      {!props.entriesOnly && <div className="git-row-actions">
        {props.onPrimary && !entry.conflicted && <button aria-label={`${props.primaryLabel} ${entry.displayPath}`} disabled={props.disabled || props.busyPath === entry.path || entry.submodule} onClick={() => props.onPrimary?.(entry)} title={entry.submodule ? "Submodule pointer operations are read-only in v1" : props.primaryLabel} type="button">{props.primaryLabel === "Stage" ? "+" : "−"}</button>}
        {props.onDiscard && !entry.conflicted && <button aria-label={`Discard ${entry.displayPath}`} disabled={props.disabled || props.busyPath === entry.path || entry.submodule} onClick={() => props.onDiscard?.(entry)} title={entry.submodule ? "Submodule pointer operations are read-only in v1" : "Discard…"} type="button">↶</button>}
      </div>}
      {entry.submodule && <small className="git-entry-note">submodule {entry.submoduleState} · actions unavailable</small>}
      {entry.displayOriginalPath && <small className="git-entry-note">{entry.indexKind === "copied" || entry.worktreeKind === "copied" ? "copied" : "renamed"} from {entry.displayOriginalPath}</small>}
      {entry.symlink && <small className="git-entry-note">symbolic link</small>}
      {entry.binary && <small className="git-entry-note">binary</small>}
      {entry.conflicted && <small className="git-entry-note">conflict {entry.conflictCode}</small>}
    </li>)}
    {visible.length < props.entries.length && <li className="git-show-more"><button onClick={() => setLimit((current) => Math.min(current + 500, props.entries.length))} type="button">Show {Math.min(500, props.entries.length - visible.length)} more…</button></li>}
  </ul></details>;
}

function GitEmpty({ title, detail, action }: { title: string; detail: string; action?: () => void }) {
  return <section className="surface-placeholder"><div className="placeholder-icon" aria-hidden="true">±</div><h2>{title}</h2><p>{detail}</p>{action && <button onClick={action} type="button">Retry</button>}</section>;
}

function groupEntries(entries: GitStatusEntry[]) {
  const visible = entries.filter((entry) => !entry.ignored);
  return {
    conflicts: visible.filter((entry) => entry.conflicted),
    staged: visible.filter((entry) => !entry.conflicted && entry.indexKind !== "none"),
    unstaged: visible.filter((entry) => !entry.conflicted && !entry.untracked && entry.worktreeKind !== "none"),
    untracked: visible.filter((entry) => entry.untracked),
    ignored: entries.filter((entry) => entry.ignored),
  };
}

function statusCode(entry: GitStatusEntry, target: GitDiffTarget): string {
  if (entry.conflicted) return "U";
  const kind = target === "staged" ? entry.indexKind : entry.worktreeKind;
  return ({ modified: "M", added: "A", deleted: "D", renamed: "R", copied: "C", typeChanged: "T", untracked: "U", ignored: "I", unmerged: "!", none: "" } as Record<string, string>)[kind] ?? "?";
}

function statusTitle(entry: GitStatusEntry, target: GitDiffTarget): string {
  if (entry.conflicted) return `Conflict ${entry.conflictCode}`;
  return target === "staged" ? entry.indexKind : entry.worktreeKind;
}

function baseName(path: string): string { return path.split("/").at(-1) || path; }
function parentName(path: string): string { const index = path.lastIndexOf("/"); return index < 0 ? "" : path.slice(0, index); }
function labelFor(kind: GitMutationRequest["kind"]): string { return ({ stageFile: "Staged", unstageFile: "Unstaged", discardFile: "Discarded", stageHunk: "Staged", unstageHunk: "Unstaged", discardHunk: "Discarded" })[kind]; }
function gitResultMessage(result: GitCommandResult, fallback: string): string {
  const command = [result.stdout.trim(), result.stderr.trim(), result.error].filter(Boolean).join(" · ") || (result.outcome === "applied" ? fallback : result.outcome === "partialOrUnknown" ? "Git outcome is partial or unknown; inspect the repository before retrying." : `Git failed with exit code ${result.exitCode}.`);
  return result.refreshFailed ? `${command} ${result.statusOmitted ? "Post-command status was omitted to keep the connection responsive" : "Status refresh failed"}: ${result.refreshError}` : command;
}
function commandDetails(result: GitCommandResult, fallback: string): string {
  return [result.stdout, result.stdoutTruncated ? "[stdout truncated]" : "", result.stderr, result.stderrTruncated ? "[stderr truncated]" : "", result.error,
    result.outcome === "partialOrUnknown" ? `Outcome is partial or unknown. HEAD ${result.preHeadOid || "?"} → ${result.postHeadOid || "?"}. Inspect the repository before retrying.` : "",
    result.refreshFailed ? `Commit completed, but ${result.statusOmitted ? "post-command status was omitted to keep the connection responsive" : "status refresh failed"}: ${result.refreshError}` : ""].filter(Boolean).join("\n") || (result.outcome === "applied" ? fallback : `Git exited with code ${result.exitCode}.`);
}

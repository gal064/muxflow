import { useEffect, useMemo, useRef, useState } from "react";
import { keyboardEventIsComposing, type CommandId } from "../../commands/registry";
import { usePublishedRowCommands, type RowCommandSource } from "../../commands/rowCommands";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { SurfaceError } from "../../ui/SurfaceError";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitCommandResult, GitDiffTarget, GitMutationRequest, GitStatusEntry, GitStatusSnapshot, GitWorkspaceClient } from "./types";
import { closePerfSpan } from "../../perf/probe";

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
  useEffect(() => {
    if (props.status) closePerfSpan("workflow.git.panelPaint");
  }, [props.status]);
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  // Stage / unstage / discard used to be a cluster of hover buttons on every
  // row. They are one right-click menu now, which is also the only way they can
  // carry a readable label instead of `+`, `−` and `↶`.
  // Deliberately no `actionable` flag in here. Whether a mutation is offered
  // depends on the connection, and the connection can drop while the menu is
  // open; a flag frozen at open time left the item enabled with `props.scope`
  // already gone, and the click handler threw.
  const [menu, setMenu] = useState<{ entry: GitStatusEntry; target: GitDiffTarget; anchor: ContextMenuAnchor }>();
  const [focusedRow, setFocusedRow] = useState<{ path: string; target: GitDiffTarget }>();
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

  // Git has no tree cursor to borrow, so the row the palette means is the last
  // one focused or right-clicked. It is stored by path rather than by object:
  // a status refresh replaces every entry, and a captured object would go on
  // describing a change that has since been staged.
  const focusedEntry = focusedRow && props.status
    ? props.status.entries.find((entry) => entry.path === focusedRow.path)
    : undefined;
  const rowActions = useMemo<readonly CommandId[]>(() => {
    if (!focusedEntry || !focusedRow) return [];
    const ids: CommandId[] = [];
    if (!focusedEntry.ignored) ids.push("git.openDiff");
    const mutable = !unavailable && !focusedEntry.conflicted && !focusedEntry.submodule
      && Boolean(props.scope) && Boolean(props.root) && Boolean(props.status);
    if (mutable) ids.push(focusedRow.target === "staged" ? "git.unstage" : "git.stage", "git.discard");
    return ids;
  }, [focusedEntry, focusedRow, props.root, props.scope, props.status, unavailable]);
  const runRowCommand = useRef<(commandId: CommandId) => void>(() => undefined);
  runRowCommand.current = (commandId) => {
    if (!focusedEntry || !focusedRow) return;
    switch (commandId) {
      case "git.openDiff": props.onOpenDiff(focusedEntry, focusedRow.target); return;
      case "git.stage": void mutateFile(focusedEntry, "unstaged", "stageFile"); return;
      case "git.unstage": void mutateFile(focusedEntry, "staged", "unstageFile"); return;
      case "git.discard":
        if (props.status && props.root && props.scope) {
          setPendingDiscard({ entry: focusedEntry, target: focusedRow.target, status: props.status, rootToken: props.root.token, connectionEpoch: props.scope.terminalEpoch });
        }
        return;
    }
  };
  const rowSource = useMemo<RowCommandSource | undefined>(() => rowActions.length === 0 || !focusedEntry ? undefined : {
    subject: focusedEntry.displayPath,
    available: rowActions,
    run: (commandId) => runRowCommand.current(commandId),
  }, [focusedEntry, rowActions]);
  usePublishedRowCommands("git", rowSource);

  if (!props.root) return <GitEmpty detail="Select a terminal pane to discover its repository." />;
  if (!props.root.gitWorktree) return <GitEmpty detail="The active pane is outside a Git worktree." />;
  if (props.loading && !props.status) return <GitEmpty detail="Reading Git status…" />;
  if (props.error && !props.status) return <GitEmpty detail={`Git unavailable: ${props.error}`} action={props.onRefresh} />;
  if (!props.status) return <GitEmpty detail="Git status is unavailable." action={props.onRefresh} />;
  if (props.status.oversized) return <GitEmpty detail={`Repository status is too large. ${props.status.error || "The host bounded this snapshot to keep the terminal connection responsive."} ${props.status.totalEntryCount ?? "Unknown"} entries were detected.`} action={props.onRefresh} />;

  const stagedCount = groups.staged.length;
  const openMenu = (entry: GitStatusEntry, target: GitDiffTarget, anchor: ContextMenuAnchor) => {
    setFocusedRow({ path: entry.path, target });
    setMenu({ entry, target, anchor });
  };
  const focusRow = (entry: GitStatusEntry, target: GitDiffTarget) => setFocusedRow({ path: entry.path, target });
  return <section className="git-sidebar" aria-label="Source Control">
    <header className="git-sidebar-header">
      <strong>{props.status.repository.headName || (props.status.repository.initial ? "Initial repository" : "Detached HEAD")}</strong>
      <small title={props.status.repository.worktreeRoot}>{props.status.repository.worktreeRoot}</small>
    </header>
    {props.error && <SurfaceError detail={props.error} />}
    {props.status.copyDetectionIncomplete && <div className="surface-note" role="status">Copy detection was bounded for this large change set; some copies may appear as additions.</div>}
    {!props.status.authoritative && <div className="surface-error" role="alert">Git status is resynchronizing. Mutations are disabled.</div>}
    <div className="git-status-groups">
      <GitGroup title="Merge changes" entries={groups.conflicts} target="unstaged" onFocusEntry={focusRow} onOpen={props.onOpenDiff} onMenu={(entry, anchor) => openMenu(entry, "unstaged", anchor)} />
      <GitGroup title="Staged" entries={groups.staged} target="staged" busyPath={busyPath} onFocusEntry={focusRow} onOpen={props.onOpenDiff} onMenu={(entry, anchor) => openMenu(entry, "staged", anchor)} />
      <GitGroup title="Changes" entries={groups.unstaged} target="unstaged" busyPath={busyPath} onFocusEntry={focusRow} onOpen={props.onOpenDiff} onMenu={(entry, anchor) => openMenu(entry, "unstaged", anchor)} />
      <GitGroup title="Untracked" entries={groups.untracked} target="unstaged" busyPath={busyPath} onFocusEntry={focusRow} onOpen={props.onOpenDiff} onMenu={(entry, anchor) => openMenu(entry, "unstaged", anchor)} />
      <GitGroup title="Ignored" entries={groups.ignored} target="unstaged" onFocusEntry={focusRow} onOpen={props.onOpenDiff} />
      {props.status.entries.length === 0 && <p className="quiet-empty">Working tree clean.</p>}
    </div>
    {/* The commit form is not permanent chrome any more: it exists exactly when
        there is something staged to commit. */}
    {stagedCount > 0 && <form className="git-commit" onSubmit={(event) => { event.preventDefault(); if (!commitComposing.current) void commit(); }}>
      <textarea aria-label="Commit message" disabled={unavailable} id="git-commit-message" onChange={(event) => setCommitMessage(event.target.value)} onCompositionEnd={() => { commitComposing.current = false; }} onCompositionStart={() => { commitComposing.current = true; }} placeholder="Commit message…" onKeyDown={(event) => {
        if (!keyboardEventIsComposing(event.nativeEvent) && event.key === "Enter" && (event.ctrlKey || event.metaKey)) { event.preventDefault(); void commit(); }
      }} value={commitMessage} />
      <button className="primary" disabled={unavailable} type="submit">Commit {stagedCount} staged</button>
      {commitError && <SurfaceError detail={commitError} />}
      {commitOutput && <pre aria-label="Git commit output" className={commitOutput.outcome === "applied" && !commitOutput.refreshFailed ? "git-output" : "git-output error"}>{commandDetails(commitOutput, "Commit created.")}</pre>}
    </form>}
    {menu && <ContextMenu
      anchor={menu.anchor}
      items={[
        { id: "open", label: "Open diff", disabled: menu.entry.ignored, run: () => props.onOpenDiff(menu.entry, menu.target) },
        ...(!unavailable && !menu.entry.conflicted && props.scope && props.root && props.status ? [
          menu.target === "staged"
            ? { id: "unstage", label: "Unstage", disabled: menu.entry.submodule, run: () => void mutateFile(menu.entry, "staged", "unstageFile") }
            : { id: "stage", label: "Stage", disabled: menu.entry.submodule, run: () => void mutateFile(menu.entry, "unstaged", "stageFile") },
          "separator" as const,
          {
            id: "discard",
            label: menu.entry.untracked ? "Delete untracked file…" : "Discard changes…",
            destructive: true,
            disabled: menu.entry.submodule,
            run: () => setPendingDiscard({ entry: menu.entry, target: menu.target, status: props.status!, rootToken: props.root!.token, connectionEpoch: props.scope!.terminalEpoch }),
          },
        ] : []),
      ]}
      label={`Actions for ${menu.entry.displayPath}`}
      onClose={() => setMenu(undefined)}
    />}
    {pendingDiscard && <ConfirmationDialog
      confirmLabel="Discard"
      destructive
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
  title: string; entries: GitStatusEntry[]; target: GitDiffTarget; busyPath?: string;
  onOpen(entry: GitStatusEntry, target: GitDiffTarget): void;
  onFocusEntry(entry: GitStatusEntry, target: GitDiffTarget): void;
  onMenu?(entry: GitStatusEntry, anchor: ContextMenuAnchor): void;
}) {
  const [limit, setLimit] = useState(200);
  if (!props.entries.length) return null;
  const visible = props.entries.slice(0, limit);
  return <section className="git-group">
    <h3 className="section-label">{props.title} · {props.entries.length}</h3>
    <ul>
      {visible.map((entry) => <li key={`${props.target}\0${entry.path}`} className={entry.conflicted ? "conflicted" : ""}>
        <button
          aria-busy={props.busyPath === entry.path}
          className="git-file"
          disabled={entry.ignored}
          onClick={() => props.onOpen(entry, props.target)}
          // What "the selected change" means for the palette and for a bound
          // shortcut: whichever row the keyboard or the pointer last landed on.
          // Both are needed — macOS WebKit does not focus a button on click.
          onFocus={() => props.onFocusEntry(entry, props.target)}
          onPointerDown={() => props.onFocusEntry(entry, props.target)}
          onContextMenu={(event) => {
            if (!props.onMenu) return;
            event.preventDefault();
            props.onMenu(entry, { x: event.clientX, y: event.clientY });
          }}
          // Stage, unstage and discard live only on that menu, so the keyboard
          // gets the same way in.
          onKeyDown={(event) => {
            if (!props.onMenu || !isContextMenuKey(event)) return;
            event.preventDefault();
            props.onMenu(entry, anchorForElement(event.currentTarget));
          }}
          title={`${entry.displayPath} · ${statusTitle(entry, props.target)}`}
          type="button"
        >
          <span className={`git-state ${statusTitle(entry, props.target)}`}>{statusCode(entry, props.target)}</span>
          <span className="git-path">{entry.displayPath}</span>
        </button>
        {entry.submodule && <small className="git-entry-note">submodule {entry.submoduleState} · actions unavailable</small>}
        {entry.displayOriginalPath && <small className="git-entry-note">{entry.indexKind === "copied" || entry.worktreeKind === "copied" ? "copied" : "renamed"} from {entry.displayOriginalPath}</small>}
        {entry.symlink && <small className="git-entry-note">symbolic link</small>}
        {entry.binary && <small className="git-entry-note">binary</small>}
        {entry.conflicted && <small className="git-entry-note">conflict {entry.conflictCode}</small>}
      </li>)}
      {visible.length < props.entries.length && <li className="git-show-more"><button onClick={() => setLimit((current) => Math.min(current + 500, props.entries.length))} type="button">Show {Math.min(500, props.entries.length - visible.length)} more…</button></li>}
    </ul>
  </section>;
}

/** One quiet line, per the brief — not an illustrated card. */
function GitEmpty({ detail, action }: { detail: string; action?: () => void }) {
  return <section aria-label="Source Control" className="git-sidebar">
    <p className="quiet-empty">{detail}{action && <> <button className="inline-action" onClick={action} type="button">Retry</button></>}</p>
  </section>;
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

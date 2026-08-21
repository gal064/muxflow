import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CommandId } from "../../commands/registry";
import { usePublishedRowCommands, type RowCommandSource } from "../../commands/rowCommands";
import { useCommittedRef } from "../../commands/useCommittedRef";
import { ConfirmationDialog } from "../../commands/ConfirmationDialog";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { Icon } from "../../ui/Icon";
import { SurfaceError } from "../../ui/SurfaceError";
import { fileIcon } from "../files/fileIcons";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import { GitCommitForm } from "./GitCommitForm";
import type { WorkspaceGitState } from "./useWorkspaceGit";
import type { GitCommandResult, GitDiffTarget, GitMutationRequest, GitStatusEntry, GitStatusSnapshot } from "./types";
import { cancelInternalPathDragSource, finishInternalPathDrag, writeInternalPathDrag } from "../terminal/internalPathDrag";

interface Props {
  /**
   * The shared repository observation. Mutations go through it so their
   * authoritative status is reconciled once, where this repository's state
   * already lives, instead of being routed back down through the application.
   */
  git: WorkspaceGitState;
  scope?: FileWorkspaceScope;
  root?: ActiveRoot;
  disabled: boolean;
  onOpenDiff(entry: GitStatusEntry, target: GitDiffTarget): void;
  onMessage(message: string): void;
}

type PendingDiscard = { entry: GitStatusEntry; target: GitDiffTarget; status: GitStatusSnapshot; rootToken: string; connectionEpoch: number };

export function GitSidebar(props: Props) {
  const [pendingDiscard, setPendingDiscard] = useState<PendingDiscard>();
  useEffect(() => {
    const dragScope = props.scope && {
      hostProfileId: props.scope.hostProfileId,
      serverIdentity: props.scope.serverIdentity,
    };
    return () => { if (dragScope) cancelInternalPathDragSource(dragScope); };
  }, [props.scope?.hostProfileId, props.scope?.serverIdentity]);
  const onRefresh = () => void props.git.handle?.refresh();
  // Stage / unstage / discard are hover buttons on the row again, by product
  // decision: the panel is VS Code's Source Control list, and there the two
  // icons that take the status letter's place on hover are how a change is
  // staged or thrown away. The right-click menu stays as the labeled path — it
  // is what carries words instead of `+`, `−` and `↶`, and it is the only way
  // in from the keyboard.
  // Deliberately no `actionable` flag in here. Whether a mutation is offered
  // depends on the connection, and the connection can drop while the menu is
  // open; a flag frozen at open time left the item enabled with `props.scope`
  // already gone, and the click handler threw.
  const [menu, setMenu] = useState<{ entry: GitStatusEntry; target: GitDiffTarget; anchor: ContextMenuAnchor }>();
  const [focusedRow, setFocusedRow] = useState<{ path: string; target: GitDiffTarget }>();
  // What is being applied to each row, shown immediately so the target of a
  // pending action is visible while the host is still the authority on whether
  // it happened. Keyed by path, so two overlapping actions cannot clobber one
  // another's indicator.
  const [pending, setPending] = useState<ReadonlyMap<string, string>>(() => new Map());
  const markPending = useCallback((path: string, label: string | undefined) => {
    setPending((current) => {
      const next = new Map(current);
      if (label === undefined) next.delete(path);
      else next.set(path, label);
      return next;
    });
  }, []);
  // Grouping is keyed on the entry list, not the snapshot: an authoritative
  // refresh that reports the same entries must not rebuild a thousand rows.
  const groups = useMemo(() => groupEntries(props.git.status?.entries ?? []), [props.git.status?.entries]);
  const unavailable = props.disabled || !props.scope || !props.root
    || !props.git.handle || !props.git.status?.authoritative;

  const mutateFile = async (entry: GitStatusEntry, target: GitDiffTarget, kind: GitMutationRequest["kind"], confirmed = false, capturedStatus = props.git.status) => {
    const observation = props.git.handle;
    if (!observation || !capturedStatus || unavailable) return;
    const request: GitMutationRequest = {
      kind, path: entry.path, ...(entry.originalPath ? { originalPath: entry.originalPath } : {}), target,
      expectedStatusGeneration: capturedStatus.generation, expectedSourceGeneration: capturedStatus.sourceGeneration,
    };
    markPending(entry.path, pendingLabelFor(kind));
    try {
      if (kind === "discardFile") {
        if (!confirmed) throw new Error("Discard was not confirmed.");
        request.confirmationToken = await observation.prepareDiscard(capturedStatus.repository.id, request);
      }
      const result = await observation.mutate(capturedStatus.repository.id, request);
      props.onMessage(gitResultMessage(result, `${labelFor(kind)} ${entry.displayPath}`));
    } catch (cause) { props.onMessage(String(cause)); }
    finally { markPending(entry.path, undefined); }
  };

  // Stable handlers for the memoized groups: a row must not be rebuilt because
  // an unrelated prop identity changed above it. Committed rather than written
  // during render, so a render React discards cannot leave these handlers
  // acting on props that were never committed.
  const latest = useCommittedRef({ ...props, unavailable });
  const openDiff = useCallback((entry: GitStatusEntry, target: GitDiffTarget) => {
    latest.current.onOpenDiff(entry, target);
  }, []);
  const focusRow = useCallback((entry: GitStatusEntry, target: GitDiffTarget) => setFocusedRow(
    // The palette only needs to know which row is selected. Re-selecting the
    // same one is not a state change, and treating it as one re-rendered every
    // group whenever the pointer crossed a row.
    (current) => current?.path === entry.path && current.target === target
      ? current
      : { path: entry.path, target },
  ), []);
  const openMenu = useCallback((entry: GitStatusEntry, target: GitDiffTarget, anchor: ContextMenuAnchor) => {
    focusRow(entry, target);
    setMenu({ entry, target, anchor });
  }, [focusRow]);
  // The hover buttons act on the row they sit on rather than on whatever the
  // palette calls "the selected change", so they need their own committed
  // handler — same shape as the palette's, one entry further along.
  const committedRowAction = (entry: GitStatusEntry, target: GitDiffTarget, action: "stage" | "unstage" | "discard") => {
    switch (action) {
      case "stage": void mutateFile(entry, "unstaged", "stageFile"); return;
      case "unstage": void mutateFile(entry, "staged", "unstageFile"); return;
      case "discard":
        // The same availability the context menu's discard checks. A connection
        // can drop between the row being drawn and the button being pressed.
        if (!unavailable && props.scope && props.root && props.git.status) {
          setPendingDiscard({ entry, target, status: props.git.status, rootToken: props.root.token, connectionEpoch: props.scope.terminalEpoch });
        }
    }
  };
  const runRowAction = useCommittedRef(committedRowAction);
  const stageRow = useCallback((entry: GitStatusEntry) => runRowAction.current(entry, "unstaged", "stage"), []);
  const unstageRow = useCallback((entry: GitStatusEntry) => runRowAction.current(entry, "staged", "unstage"), []);
  const discardRow = useCallback((entry: GitStatusEntry, target: GitDiffTarget) => runRowAction.current(entry, target, "discard"), []);
  const commit = useCallback(async (message: string) => {
    const { git } = latest.current;
    const status = git.status;
    // The same condition the form's controls are disabled by. A guard that is
    // weaker than its own control is a guard that does not hold.
    if (!status || !git.handle || latest.current.unavailable) return undefined;
    const result = await git.handle.commit(status.repository.id, status.generation, message);
    latest.current.onMessage(gitResultMessage(result, result.outcome === "applied" ? "Commit created." : "Commit failed."));
    return result;
  }, []);

  // Git has no tree cursor to borrow, so the row the palette means is the last
  // one focused or right-clicked. It is stored by path rather than by object:
  // a status refresh replaces every entry, and a captured object would go on
  // describing a change that has since been staged.
  const focusedEntry = focusedRow && props.git.status
    ? props.git.status.entries.find((entry) => entry.path === focusedRow.path)
    : undefined;
  const rowActions = useMemo<readonly CommandId[]>(() => {
    if (!focusedEntry || !focusedRow) return [];
    const ids: CommandId[] = ["git.openDiff"];
    const mutable = !unavailable && !focusedEntry.conflicted && !focusedEntry.submodule
      && Boolean(props.scope) && Boolean(props.root) && Boolean(props.git.status);
    if (mutable) ids.push(focusedRow.target === "staged" ? "git.unstage" : "git.stage", "git.discard");
    return ids;
  }, [focusedEntry, focusedRow, props.root, props.scope, props.git.status, unavailable]);
  // Committed, not written during render: the sibling Explorer tree names the
  // render-time write as wrong for this exact publication, and it was wrong
  // here too — a render React discards still runs its body, and the palette
  // would then hold a closure over a focused row that was never committed.
  const committedRowCommand = (commandId: CommandId) => {
    if (!focusedEntry || !focusedRow) return;
    switch (commandId) {
      case "git.openDiff": props.onOpenDiff(focusedEntry, focusedRow.target); return;
      case "git.stage": void mutateFile(focusedEntry, "unstaged", "stageFile"); return;
      case "git.unstage": void mutateFile(focusedEntry, "staged", "unstageFile"); return;
      case "git.discard":
        if (props.git.status && props.root && props.scope) {
          setPendingDiscard({ entry: focusedEntry, target: focusedRow.target, status: props.git.status, rootToken: props.root.token, connectionEpoch: props.scope.terminalEpoch });
        }
        return;
    }
  };
  const runRowCommand = useCommittedRef(committedRowCommand);
  const rowSource = useMemo<RowCommandSource | undefined>(() => rowActions.length === 0 || !focusedEntry ? undefined : {
    subject: focusedEntry.displayPath,
    available: rowActions,
    run: (commandId) => runRowCommand.current(commandId),
  }, [focusedEntry, rowActions]);
  usePublishedRowCommands("git", rowSource);

  if (!props.root) return <GitEmpty detail="Select a terminal pane to discover its repository." />;
  if (!props.root.gitWorktree) return <GitEmpty detail="The active pane is outside a Git worktree." />;
  if (props.git.loading && !props.git.status) return <GitEmpty detail="Reading Git status…" />;
  if (props.git.error && !props.git.status) return <GitEmpty detail={`Git unavailable: ${props.git.error}`} action={onRefresh} />;
  if (!props.git.status) return <GitEmpty detail="Git status is unavailable." action={onRefresh} />;
  if (props.git.status.oversized) return <GitEmpty detail={`Repository status is too large. ${props.git.status.error || "The host bounded this snapshot to keep the terminal connection responsive."} ${props.git.status.totalEntryCount ?? "Unknown"} entries were detected.`} action={onRefresh} />;

  const stagedCount = groups.staged.length;
  const dragEntry = props.scope ? (entry: GitStatusEntry, transfer: DataTransfer) => {
    if (entry.absolutePath) writeInternalPathDrag(transfer, {
      hostProfileId: props.scope!.hostProfileId,
      serverIdentity: props.scope!.serverIdentity,
      path: entry.absolutePath,
    });
  } : undefined;
  return <section className="git-sidebar" aria-label="Source Control">
    <header className="git-sidebar-header">
      <span className="git-sidebar-identity">
        <strong>{props.git.status.repository.headName || (props.git.status.repository.initial ? "Initial repository" : "Detached HEAD")}</strong>
        <small title={props.git.status.repository.worktreeRoot}>{props.git.status.repository.worktreeRoot}</small>
      </span>
      {/* Refresh left the diff toolbar, where it re-read one file. Here it is
          the panel's own control and re-reads the repository. */}
      <button aria-label="Refresh Git status" className="bar-button" onClick={onRefresh} title="Refresh Git status" type="button"><Icon name="refresh" /></button>
    </header>
    {props.git.error && <SurfaceError detail={props.git.error} />}
    {props.git.status.copyDetectionIncomplete && <div className="surface-note" role="status">Copy detection was bounded for this large change set; some copies may appear as additions.</div>}
    {!props.git.status.authoritative && <div className="surface-error" role="alert">Git status is resynchronizing. Mutations are disabled.</div>}
    <div className="git-status-groups">
      <GitGroup title="Merge changes" entries={groups.conflicts} target="unstaged" mutable={!unavailable} onDiscard={discardRow} onDrag={dragEntry} onFocusEntry={focusRow} onOpen={openDiff} onMenu={openMenu} onStage={stageRow} onUnstage={unstageRow} />
      <GitGroup title="Staged" entries={groups.staged} target="staged" mutable={!unavailable} pending={pending} onDiscard={discardRow} onDrag={dragEntry} onFocusEntry={focusRow} onOpen={openDiff} onMenu={openMenu} onStage={stageRow} onUnstage={unstageRow} />
      <GitGroup title="Changes" entries={groups.unstaged} target="unstaged" mutable={!unavailable} pending={pending} onDiscard={discardRow} onDrag={dragEntry} onFocusEntry={focusRow} onOpen={openDiff} onMenu={openMenu} onStage={stageRow} onUnstage={unstageRow} />
      <GitGroup title="Untracked" entries={groups.untracked} target="unstaged" mutable={!unavailable} pending={pending} onDiscard={discardRow} onDrag={dragEntry} onFocusEntry={focusRow} onOpen={openDiff} onMenu={openMenu} onStage={stageRow} onUnstage={unstageRow} />
      {props.git.status.entries.length === 0 && <p className="quiet-empty">Working tree clean.</p>}
    </div>
    {/* The commit form is not permanent chrome any more: it exists exactly when
        there is something staged to commit. */}
    {stagedCount > 0 && <GitCommitForm commit={commit} disabled={unavailable} stagedCount={stagedCount} />}
    {menu && <ContextMenu
      anchor={menu.anchor}
      items={[
        { id: "open", label: "Open diff", run: () => props.onOpenDiff(menu.entry, menu.target) },
        ...(!unavailable && !menu.entry.conflicted && props.scope && props.root && props.git.status ? [
          menu.target === "staged"
            ? { id: "unstage", label: "Unstage", disabled: menu.entry.submodule, run: () => void mutateFile(menu.entry, "staged", "unstageFile") }
            : { id: "stage", label: "Stage", disabled: menu.entry.submodule, run: () => void mutateFile(menu.entry, "unstaged", "stageFile") },
          "separator" as const,
          {
            id: "discard",
            label: menu.entry.untracked ? "Delete untracked file…" : "Discard changes…",
            destructive: true,
            disabled: menu.entry.submodule,
            run: () => setPendingDiscard({ entry: menu.entry, target: menu.target, status: props.git.status!, rootToken: props.root!.token, connectionEpoch: props.scope!.terminalEpoch }),
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
        if (props.root?.token !== captured.rootToken || props.scope?.terminalEpoch !== captured.connectionEpoch || props.git.status?.repository.id !== captured.status.repository.id) {
          props.onMessage("Discard was cancelled because the repository connection changed.");
          return;
        }
        void mutateFile(captured.entry, captured.target, "discardFile", true, captured.status);
      }}
      title={pendingDiscard.entry.untracked ? "Delete untracked file?" : "Discard file changes?"}
    />}
  </section>;
}

const GitGroup = memo(function GitGroup(props: {
  title: string; entries: GitStatusEntry[]; target: GitDiffTarget; pending?: ReadonlyMap<string, string>;
  /** Whether this repository can be written to at all; a row decides the rest. */
  mutable?: boolean;
  onOpen(entry: GitStatusEntry, target: GitDiffTarget): void;
  onFocusEntry(entry: GitStatusEntry, target: GitDiffTarget): void;
  onMenu?(entry: GitStatusEntry, target: GitDiffTarget, anchor: ContextMenuAnchor): void;
  onStage?(entry: GitStatusEntry): void;
  onUnstage?(entry: GitStatusEntry): void;
  onDiscard?(entry: GitStatusEntry, target: GitDiffTarget): void;
  onDrag?(entry: GitStatusEntry, transfer: DataTransfer): void;
}) {
  const [limit, setLimit] = useState(200);
  if (!props.entries.length) return null;
  const visible = props.entries.slice(0, limit);
  const staged = props.target === "staged";
  return <section className="git-group">
    <h3 className="section-label">{props.title} · {props.entries.length}</h3>
    <ul>
      {visible.map((entry) => {
        // The row is a filename first, the way VS Code's is: the basename in
        // full, the directory beside it as dimmed context that is allowed to be
        // the part that gets truncated.
        const slash = entry.displayPath.lastIndexOf("/");
        const name = slash < 0 ? entry.displayPath : entry.displayPath.slice(slash + 1);
        const dir = slash < 0 ? "" : entry.displayPath.slice(0, slash);
        const glyph = fileIcon({ name, kind: "file" });
        // A conflict has to be resolved elsewhere, and a submodule pointer is
        // read-only, so neither offers the buttons at all rather than offering
        // them disabled.
        const offersActions = Boolean(props.mutable) && !entry.conflicted && !entry.submodule;
        return <li key={`${props.target}\0${entry.path}`} className={entry.conflicted ? "conflicted" : ""}>
          <button
            aria-busy={props.pending?.has(entry.path) ?? false}
            className="git-file"
            draggable={Boolean(props.onDrag && entry.absolutePath)}
            onDragStart={(event) => entry.absolutePath && props.onDrag?.(entry, event.dataTransfer)}
            onDragEnd={finishInternalPathDrag}
            onClick={() => props.onOpen(entry, props.target)}
            // What "the selected change" means for the palette and for a bound
            // shortcut: whichever row the keyboard or the pointer last landed on.
            // Both are needed — macOS WebKit does not focus a button on click.
            onFocus={() => props.onFocusEntry(entry, props.target)}
            onPointerDown={() => props.onFocusEntry(entry, props.target)}
            onContextMenu={(event) => {
              if (!props.onMenu) return;
              event.preventDefault();
              props.onMenu(entry, props.target, { x: event.clientX, y: event.clientY });
            }}
            // The hover buttons are a pointer affordance, so the menu is how the
            // keyboard reaches the same three actions.
            onKeyDown={(event) => {
              if (!props.onMenu || !isContextMenuKey(event)) return;
              event.preventDefault();
              props.onMenu(entry, props.target, anchorForElement(event.currentTarget));
            }}
            title={`${entry.displayPath} · ${statusTitle(entry, props.target)}`}
            type="button"
          >
            <span className="git-file-icon" style={{ color: glyph.color }}><Icon name={glyph.icon} size={12} /></span>
            <span className="git-file-name">{name}</span>
            {dir !== "" && <span className="git-file-dir">{dir}</span>}
            <span className={`git-state ${statusTitle(entry, props.target)}`}>{statusCode(entry, props.target)}</span>
          </button>
          {offersActions && <span className="git-row-actions">
            <button
              aria-label={staged ? "Unstage file" : "Stage file"}
              onClick={() => (staged ? props.onUnstage : props.onStage)?.(entry)}
              title={staged ? "Unstage file" : "Stage file"}
              type="button"
            ><Icon name={staged ? "minus" : "plus"} size={12} /></button>
            <button
              aria-label={entry.untracked ? "Delete untracked file" : "Discard changes"}
              onClick={() => props.onDiscard?.(entry, props.target)}
              title={entry.untracked ? "Delete untracked file" : "Discard changes"}
              type="button"
            ><Icon name="discard" size={12} /></button>
          </span>}
          {props.pending?.get(entry.path) && <small className="git-entry-note">{props.pending.get(entry.path)}</small>}
          {entry.submodule && <small className="git-entry-note">submodule {entry.submoduleState} · actions unavailable</small>}
          {entry.displayOriginalPath && <small className="git-entry-note">{entry.indexKind === "copied" || entry.worktreeKind === "copied" ? "copied" : "renamed"} from {entry.displayOriginalPath}</small>}
          {entry.symlink && <small className="git-entry-note">symbolic link</small>}
          {entry.conflicted && <small className="git-entry-note">conflict {entry.conflictCode}</small>}
        </li>;
      })}
      {visible.length < props.entries.length && <li className="git-show-more"><button onClick={() => setLimit((current) => Math.min(current + 500, props.entries.length))} type="button">Show {Math.min(500, props.entries.length - visible.length)} more…</button></li>}
    </ul>
  </section>;
});

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
/** What is being done to a row, while the host is still deciding whether it happened. */
function pendingLabelFor(kind: GitMutationRequest["kind"]): string { return ({ stageFile: "staging…", unstageFile: "unstaging…", discardFile: "discarding…", stageHunk: "staging hunk…", unstageHunk: "unstaging hunk…", discardHunk: "discarding hunk…" })[kind]; }
function gitResultMessage(result: GitCommandResult, fallback: string): string {
  const command = [result.stdout.trim(), result.stderr.trim(), result.error].filter(Boolean).join(" · ") || (result.outcome === "applied" ? fallback : result.outcome === "partialOrUnknown" ? "Git outcome is partial or unknown; inspect the repository before retrying." : `Git failed with exit code ${result.exitCode}.`);
  return result.refreshFailed ? `${command} ${result.statusOmitted ? "Post-command status was omitted to keep the connection responsive" : "Status refresh failed"}: ${result.refreshError}` : command;
}

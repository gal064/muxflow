import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { CommandId } from "../../commands/registry";
import { usePublishedRowCommands, type RowCommandSource } from "../../commands/rowCommands";
import { useModalDialog } from "../../commands/useModalDialog";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { Icon } from "../../ui/Icon";
import { SurfaceError } from "../../ui/SurfaceError";
import type { DownloadIntent } from "./downloadFlow";
import { DownloadTransfers } from "./DownloadTransfers";
import { fileIcon } from "./fileIcons";
import type { ActiveRoot, DirectoryListing, FileEntry, FileMutation, TransferStatus } from "./types";
import { closePerfSpan, recordPerfHighWater } from "../../perf/probe";

interface Props {
  root?: ActiveRoot;
  listings: ReadonlyMap<string, DirectoryListing>;
  expanded: ReadonlySet<string>;
  loading: ReadonlySet<string>;
  transfers: readonly TransferStatus[];
  /** Reads the user asked for and is waiting on. See `useWorkspaceFiles.refresh`. */
  requestedReads: number;
  /**
   * Absolute paths git reports as ignored, or `undefined` when there is no
   * authoritative answer — no worktree, an oversized status, a repository that
   * failed to report. Undefined means "show everything": the tree never hides
   * a file on a guess.
   *
   * Git collapses an ignored directory to one record, so a path in here hides
   * the entry itself *and* everything beneath it.
   */
  ignoredPaths?: ReadonlySet<string>;
  scopeIdentity: string;
  disabled: boolean;
  error?: string;
  onToggle(path: string): void;
  /**
   * `preview: true` is a single click — a disposable tab the next single click
   * reuses. Every deliberate open (double-click, Enter, the Open item, the
   * palette command) asks for a permanent one.
   */
  onOpen(entry: FileEntry, options: { preview: boolean }): void;
  onMutate(mutation: FileMutation): Promise<void>;
  /**
   * What to download, not how: the collision policy is the save panel's
   * business now, and the tree has no business pre-deciding it.
   */
  onDownload(intent: DownloadIntent): Promise<void>;
  onCancelTransfer(id: string): Promise<void>;
  onRefresh(path?: string): void;
  onLoadMore(path: string): void;
}

type PendingAction = { action: "newFile" | "newDirectory" | "rename" | "move" | "duplicate" | "delete"; rootToken: string; scopeIdentity: string; entry?: FileEntry };

export function ExplorerTree(props: Props) {
  // One right-click menu replaces the per-row `•••` button that used to appear
  // on hover, and the three header buttons above it. Nothing in this tree is a
  // resting control any more.
  const [menu, setMenu] = useState<{ entry?: FileEntry; anchor: ContextMenuAnchor }>();
  const [pending, setPending] = useState<PendingAction>();
  const [value, setValue] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const [nonEmptyOverwrite, setNonEmptyOverwrite] = useState(false);
  const [dialogError, setDialogError] = useState<string>();
  const [focusIndex, setFocusIndex] = useState(0);
  // VS Code's escape hatch, and the reason hiding them is safe: the rule is
  // reversible from the tree itself, without a settings trip.
  const [showIgnored, setShowIgnored] = useState(false);
  const treeRef = useRef<HTMLDivElement>(null);
  const composing = useRef(false);
  const dialogTitleId = useId();
  const closeDialog = () => setPending(undefined);
  const dialogRef = useModalDialog<HTMLFormElement>(closeDialog, Boolean(pending));
  const rootName = props.root?.path.split("/").filter(Boolean).at(-1) ?? props.root?.path ?? "No active root";
  const hidden = showIgnored ? undefined : props.ignoredPaths;
  const rows = useMemo(() => props.root ? flattenTree(props.root.path, props.listings, props.expanded, hidden) : [], [hidden, props.expanded, props.listings, props.root]);
  useEffect(() => {
    recordPerfHighWater("explorer.domRows", rows.length);
    closePerfSpan("explorer.expandToPaint");
    closePerfSpan("explorer.externalChangeToPaint");
    closePerfSpan("workflow.explorer.rootPaint");
    closePerfSpan("workflow.explorer.directoryExpandPaint");
  }, [rows]);
  useEffect(() => setFocusIndex((current) => Math.min(current, Math.max(0, rows.length - 1))), [rows.length]);
  // A new root is a new repository, and the toggle is not offered when that
  // repository has nothing ignored — so a `true` carried across would leave
  // ignored files showing with no visible reason and no way to put them back.
  useEffect(() => { setPending(undefined); setShowIgnored(false); }, [props.root?.token, props.scopeIdentity]);

  const focusRow = (index: number) => {
    const next = Math.max(0, Math.min(rows.length - 1, index));
    setFocusIndex(next);
    window.requestAnimationFrame(() => treeRef.current?.querySelector<HTMLElement>(`[data-tree-index="${next}"]`)?.focus());
  };

  const navigateEntry = (event: KeyboardEvent<HTMLDivElement>, index: number, depth: number, entry: FileEntry) => {
    if (event.target !== event.currentTarget) return;
    // Every file action is on the context menu, so the keyboard needs a way to
    // open it or a keyboard-only user cannot rename, move or delete anything.
    if (isContextMenuKey(event)) {
      event.preventDefault();
      setMenu({ entry, anchor: anchorForElement(event.currentTarget) });
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault(); focusRow(index + (event.key === "ArrowDown" ? 1 : -1)); return;
    }
    if (event.key === "ArrowRight") {
      event.preventDefault();
      if (entry.expandable && !props.expanded.has(entry.path)) props.onToggle(entry.path);
      else if (rows[index + 1]?.depth === depth + 1) focusRow(index + 1);
      return;
    }
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      if (entry.expandable && props.expanded.has(entry.path)) props.onToggle(entry.path);
      else {
        for (let parent = index - 1; parent >= 0; parent -= 1) {
          if (rows[parent].depth < depth) { focusRow(parent); break; }
        }
      }
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      // Enter pins, as VS Code's does: reaching a file with the keyboard and
      // pressing Enter is as deliberate as a double-click.
      if (entry.expandable) props.onToggle(entry.path); else props.onOpen(entry, { preview: false });
    }
  };

  const begin = (action: PendingAction["action"], entry?: FileEntry) => {
    if (!props.root) return;
    setPending({ action, entry, rootToken: props.root.token, scopeIdentity: props.scopeIdentity });
    setOverwrite(false);
    setNonEmptyOverwrite(false);
    setDialogError(undefined);
    setValue(action === "rename" || action === "duplicate" ? entry?.path ?? "" : "");
    setMenu(undefined);
  };

  // The row the palette means is the one the tree has focus on — the same row
  // its arrow keys walk and its Shift+F10 opens a menu for. Nothing new to aim
  // with, and the disabled state published here is the same one the menu draws.
  //
  // `onPointerDown` on the row is why clicking counts as pointing at it: macOS
  // WebKit does not focus a button on click (that is the platform convention),
  // so without it the palette went on offering actions for whichever row the
  // keyboard last visited while the user was clicking a different one.
  const focusedRow = rows[focusIndex];
  const focusedEntry = focusedRow?.kind === "entry" ? focusedRow.entry : undefined;
  const rowActions = useMemo<readonly CommandId[]>(() => {
    if (!props.root) return [];
    const ids: CommandId[] = [];
    if (focusedEntry) {
      if (focusedEntry.kind !== "directory") ids.push("files.open");
      ids.push("files.download");
      if (!props.disabled) ids.push("files.rename", "files.move", "files.duplicate", "files.delete");
    }
    if (!props.disabled) ids.push("files.newFile", "files.newFolder");
    ids.push("files.refresh");
    return ids;
  }, [focusedEntry, props.disabled, props.root]);
  // A ref, not a dependency: the handlers close over state that changes every
  // keystroke, and rebuilding the published source that often would be churn
  // for nothing. What the palette needs to be current is the *id list*, and
  // that is memoized above.
  const runRowCommand = useRef<(commandId: CommandId) => void>(() => undefined);
  runRowCommand.current = (commandId) => {
    switch (commandId) {
      case "files.open": if (focusedEntry) props.onOpen(focusedEntry, { preview: false }); return;
      case "files.rename": begin("rename", focusedEntry); return;
      case "files.move": begin("move", focusedEntry); return;
      case "files.duplicate": begin("duplicate", focusedEntry); return;
      case "files.delete": begin("delete", focusedEntry); return;
      case "files.download": if (focusedEntry) void props.onDownload({ path: focusedEntry.path, kind: focusedEntry.kind === "directory" ? "folder" : "file" }); return;
      case "files.newFile": begin("newFile", focusedEntry); return;
      case "files.newFolder": begin("newDirectory", focusedEntry); return;
      case "files.refresh": props.onRefresh(); return;
    }
  };
  const rowSource = useMemo<RowCommandSource | undefined>(() => rowActions.length === 0 ? undefined : {
    subject: focusedEntry?.name ?? rootName,
    available: rowActions,
    run: (commandId) => runRowCommand.current(commandId),
  }, [focusedEntry, rootName, rowActions]);
  usePublishedRowCommands("files", rowSource);

  const submit = async () => {
    if (!pending || !props.root) return;
    if (pending.rootToken !== props.root.token || pending.scopeIdentity !== props.scopeIdentity || props.disabled) {
      setDialogError("This file action was cancelled because its host or active root changed.");
      return;
    }
    try {
      const entry = pending.entry;
      if ((pending.action === "newFile" || pending.action === "newDirectory") && value.trim()) {
        await props.onMutate({ kind: pending.action === "newFile" ? "createFile" : "createDirectory", parent: entry?.kind === "directory" ? entry.path : props.root.path, name: value.trim() });
      } else if (entry && pending.action === "rename" && value.trim()) {
        await props.onMutate({ kind: "rename", path: entry.path, destination: value.trim(), overwrite, confirmedNonEmpty: nonEmptyOverwrite });
      } else if (entry && pending.action === "move" && value.trim()) {
        await props.onMutate({ kind: "move", path: entry.path, destination: value.trim(), overwrite, confirmedNonEmpty: nonEmptyOverwrite });
      } else if (entry && pending.action === "duplicate" && value.trim()) {
        await props.onMutate({ kind: "duplicate", path: entry.path, destination: value.trim(), overwrite, confirmedNonEmpty: nonEmptyOverwrite });
      } else if (entry && pending.action === "delete") {
        await props.onMutate({ kind: "delete", path: entry.path, confirmedNonEmpty: entry.kind === "directory" });
      }
      setPending(undefined);
    } catch (error) {
      setDialogError(String(error));
    }
  };

  // Offered only when git has actually told us something to hide: without an
  // authoritative status the tree already shows everything, and a toggle that
  // changes nothing is worse than none.
  //
  // It is on *both* menus deliberately. The header is not a focusable element,
  // so a header-only item would make the single escape hatch out of a feature
  // that hides content by default reachable by mouse alone; Shift+F10 on any
  // row reaches the entry menu.
  const ignoredToggle = props.ignoredPaths?.size
    ? [{ id: "ignored", label: showIgnored ? "Hide ignored files" : "Show ignored files", run: () => setShowIgnored((current) => !current) }]
    : [];

  return <div className="explorer-tree">
    <header className="explorer-root" onContextMenu={(event) => {
      event.preventDefault();
      setMenu({ anchor: { x: event.clientX, y: event.clientY } });
    }}>
      <span title={props.root?.path}>{rootName}</span>
      {props.root && <small>{props.root.gitWorktree ? "git worktree" : "pane cwd"}</small>}
      {/* The one wait that is shown for a listing already on screen, and it is
          drawn here rather than in the tree because the header is outside the
          scrolling box: nothing it does can change the tree's content height,
          which is what made the row-shaped version flicker. Only reads the user
          asked for reach this — a refresh nobody requested stays silent. */}
      {props.requestedReads > 0 && <small className="explorer-refreshing" role="status">Refreshing…</small>}
    </header>
    {props.error && <SurfaceError detail={props.error} />}
    <div
      aria-busy={props.loading.size > 0 ? true : undefined}
      aria-label="Files"
      className="file-tree"
      onContextMenu={(event) => {
        // Right-clicking the empty area below the tree acts on the root, which
        // is where "new file" and "refresh" went when the header buttons did.
        if (event.target !== event.currentTarget) return;
        event.preventDefault();
        setMenu({ anchor: { x: event.clientX, y: event.clientY } });
      }}
      ref={treeRef}
      role="tree"
    >
      {rows.map((row, index) => {
        if (row.kind === "more") return <button aria-level={row.depth + 1} className="load-more-files" data-tree-index={index} disabled={props.loading.has(row.directory)} key={`more:${row.directory}`} onClick={() => props.onLoadMore(row.directory)} onFocus={() => setFocusIndex(index)} onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); focusRow(index + (event.key === "ArrowDown" ? 1 : -1)); }
        }} role="treeitem" style={{ marginLeft: `${8 + row.depth * 14}px` }} tabIndex={index === focusIndex ? 0 : -1} type="button">Load more…</button>;
        const { entry, depth } = row;
        const isOpen = props.expanded.has(entry.path);
        const icon = fileIcon(entry, isOpen);
        return <div aria-expanded={entry.expandable ? isOpen : undefined} aria-level={depth + 1} aria-selected={index === focusIndex} className="file-row" data-tree-index={index} key={entry.path} onClick={(event) => { if (event.target === event.currentTarget) entry.expandable ? props.onToggle(entry.path) : props.onOpen(entry, { preview: true }); }} onDoubleClick={(event) => {
          // The row's indent strip is outside the button but inside the row,
          // so without this a file reached by clicking its padding could be
          // previewed forever and never pinned.
          if (event.target === event.currentTarget && !entry.expandable) props.onOpen(entry, { preview: false });
        }} onContextMenu={(event) => {
          event.preventDefault();
          focusRow(index);
          setMenu({ entry, anchor: { x: event.clientX, y: event.clientY } });
        }} onFocus={() => setFocusIndex(index)} onKeyDown={(event) => navigateEntry(event, index, depth, entry)} onPointerDown={() => setFocusIndex(index)} role="treeitem" style={{ paddingLeft: `${8 + depth * 14}px` }} tabIndex={index === focusIndex ? 0 : -1}>
          {/* The click of a double-click fires first and opens the preview;
              the second click then pins that same tab, which is exactly the
              VS Code behaviour and needs no click-delay timer. */}
          <button className="file-main" onClick={() => entry.expandable ? props.onToggle(entry.path) : props.onOpen(entry, { preview: true })} onDoubleClick={() => { if (!entry.expandable) props.onOpen(entry, { preview: false }); }} tabIndex={-1} type="button">
            <span className="file-twisty">{entry.expandable ? <Icon name={isOpen ? "chevronDown" : "chevronRight"} size={11} /> : null}</span>
            <span className={`file-icon ${entry.kind}`} style={{ color: icon.color }}><Icon name={icon.icon} size={14} /></span>
            <span title={entryTooltip(entry)}>{entry.name}</span>
          </button>
        </div>;
      })}
      {/* Only until this directory has answered once — "we have no listing yet",
          not "we have no rows", so a directory that is genuinely empty does not
          swap between these two lines every time it is re-read either.

          These live inside the scrolling box, so showing one for a refresh of a
          listing already on screen grew the content by a row and shrank it again
          on every filesystem event: the list flickering on a short listing, and
          the overlay scrollbars revealing and re-hiding on a long one. A refresh
          is `aria-busy` on the tree instead — the same fact, no layout, and no
          live region re-announcing "Loading…" once per event. */}
      {props.root && !props.listings.has(props.root.path) && props.loading.has(props.root.path) && <p className="quiet-empty" role="status">Loading…</p>}
      {!props.root && <p className="quiet-empty">Select a live terminal pane.</p>}
      {/* "Empty" is now a claim about the *filtered* rows, so it has to
          distinguish the two ways of having none: the directory really has
          nothing in it, or everything in it is ignored and the tree is the
          reason it looks bare. Saying "empty" for the second is a lie that
          sends people looking for a filesystem problem. */}
      {props.root && props.listings.has(props.root.path) && rows.length === 0 && <p className="quiet-empty">
        {hidden && (props.listings.get(props.root.path)?.entries.length ?? 0) > 0
          ? "Everything here is ignored by git. Right-click the Explorer header to show ignored files."
          : "This directory is empty."}
      </p>}
    </div>
    {menu && <ContextMenu
      anchor={menu.anchor}
      items={menu.entry
        ? [
          ...(menu.entry.kind === "directory" ? [] : [{ id: "open", label: "Open", run: () => props.onOpen(menu.entry!, { preview: false }) }]),
          { id: "rename", label: "Rename…", disabled: props.disabled, run: () => begin("rename", menu.entry) },
          { id: "move", label: "Move…", disabled: props.disabled, run: () => { setValue(""); begin("move", menu.entry); } },
          { id: "duplicate", label: "Duplicate…", disabled: props.disabled, run: () => begin("duplicate", menu.entry) },
          { id: "download", label: menu.entry.kind === "directory" ? "Download folder…" : "Download…", run: () => void props.onDownload({ path: menu.entry!.path, kind: menu.entry!.kind === "directory" ? "folder" : "file" }) },
          "separator" as const,
          { id: "newFile", label: "New file…", disabled: props.disabled || !props.root, run: () => begin("newFile", menu.entry) },
          { id: "newDirectory", label: "New folder…", disabled: props.disabled || !props.root, run: () => begin("newDirectory", menu.entry) },
          "separator" as const,
          { id: "delete", label: "Delete…", destructive: true, disabled: props.disabled, run: () => begin("delete", menu.entry) },
          ...(ignoredToggle.length > 0 ? ["separator" as const, ...ignoredToggle] : []),
        ]
        : [
          { id: "newFile", label: "New file…", disabled: props.disabled || !props.root, run: () => begin("newFile") },
          { id: "newDirectory", label: "New folder…", disabled: props.disabled || !props.root, run: () => begin("newDirectory") },
          "separator" as const,
          ...ignoredToggle,
          { id: "refresh", label: "Refresh", run: () => props.onRefresh() },
        ]}
      label={menu.entry ? `Actions for ${menu.entry.name}` : "Explorer actions"}
      onClose={() => setMenu(undefined)}
    />}
    <DownloadTransfers onCancelTransfer={props.onCancelTransfer} transfers={props.transfers} />
    {pending && <div className="modal-backdrop" role="presentation"><form aria-labelledby={dialogTitleId} aria-modal="true" className="file-dialog confirmation" onSubmit={(event) => { event.preventDefault(); if (!composing.current) void submit(); }} ref={dialogRef} role="dialog">
      <h2 id={dialogTitleId}>{labelForAction(pending.action)}</h2>
      {pending.action === "delete" ? <p>Delete <code>{pending.entry?.path}</code>? {pending.entry?.kind === "directory" && "Non-empty directories require this confirmation."}</p> : <label>
        {pending.action.startsWith("new") ? "Name" : "Destination path"}
        <input autoFocus onChange={(event) => setValue(event.target.value)} onCompositionEnd={() => { composing.current = false; }} onCompositionStart={() => { composing.current = true; }} value={value} />
      </label>}
      {["rename", "move", "duplicate"].includes(pending.action) && <label className="overwrite"><input checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} type="checkbox" /> Allow overwrite after confirmation</label>}
      {["rename", "move", "duplicate"].includes(pending.action) && overwrite && <label className="overwrite"><input checked={nonEmptyOverwrite} onChange={(event) => setNonEmptyOverwrite(event.target.checked)} type="checkbox" /> Also replace a non-empty destination directory</label>}
      {dialogError && <SurfaceError detail={dialogError} />}
      <div className="dialog-actions"><button onClick={() => setPending(undefined)} type="button">Cancel</button><button className={pending.action === "delete" ? "danger" : "primary"} type="submit">{pending.action === "delete" ? "Delete" : "Apply"}</button></div>
    </form></div>}
  </div>;
}

function flattenTree(
  root: string,
  listings: ReadonlyMap<string, DirectoryListing>,
  expanded: ReadonlySet<string>,
  ignored?: ReadonlySet<string>,
) {
  const rows: ({ kind: "entry"; entry: FileEntry; depth: number } | { kind: "more"; directory: string; depth: number })[] = [];
  // Git reports `target/` once and never its ten thousand contents, which
  // membership alone would miss — except that this walk only ever descends
  // into a directory it has already decided to keep, so a dropped directory
  // takes its whole subtree with it and there is nothing left to match. The
  // set is absolute paths from the worktree root, and the Explorer root *is*
  // the worktree root: the host refuses a status for anything else
  // (`discover_repository`, `apps/host/src/service/git/status.rs`), so no row
  // can sit under an ignored ancestor this walk never saw.
  const visit = (directory: string, depth: number) => {
    const listing = listings.get(directory);
    for (const entry of listing?.entries ?? []) {
      if (ignored?.has(entry.path)) continue;
      rows.push({ kind: "entry", entry, depth });
      if (entry.expandable && expanded.has(entry.path)) visit(entry.path, depth + 1);
    }
    if (listing && !listing.complete && listing.nextPageToken) rows.push({ kind: "more", directory, depth });
  };
  visit(root, 0);
  return rows;
}

function labelForAction(action: PendingAction["action"]): string {
  return ({ newFile: "Create file", newDirectory: "Create folder", rename: "Rename", move: "Move", duplicate: "Duplicate", delete: "Delete" } as const)[action];
}

function entryTooltip(entry: FileEntry): string {
  const modified = Number(entry.modifiedMillis);
  const lines = [entry.path, `${entry.kind} · ${entry.sizeBytes} bytes`];
  if (Number.isFinite(modified) && modified > 0) lines.push(`Modified ${new Date(modified).toLocaleString()}`);
  if (entry.symlinkTarget) lines.push(`Symlink → ${entry.symlinkTarget}`);
  return lines.join("\n");
}

import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import type { CommandId } from "../../commands/registry";
import { usePublishedRowCommands, type RowCommandSource } from "../../commands/rowCommands";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { SurfaceError } from "../../ui/SurfaceError";
import type { DownloadIntent } from "./downloadFlow";
import { DownloadTransfers } from "./DownloadTransfers";
import { ExplorerMutationDialog, type PendingMutation } from "./ExplorerMutationDialog";
import { ExplorerEntryRow, ExplorerMoreRow, type ExplorerRowActions } from "./ExplorerRow";
import { DEFAULT_ROW_HEIGHT, mountedRowCount, rowWindow, scrollOffsetForRow } from "./explorerWindow";
import type { ActiveRoot, DirectoryListing, FileEntry, FileMutation, TransferStatus } from "./types";
import { recordPerfHighWater } from "../../perf/probe";

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

/**
 * What a row's actions do before the first commit: nothing.
 *
 * A real object rather than a cast, because the ref is not optional — it is
 * uninitialized for exactly one render, during which no event can reach a row.
 */
const INERT_ROW_ACTIONS: ExplorerRowActions = {
  toggle: () => undefined,
  open: () => undefined,
  focus: () => undefined,
  contextMenu: () => undefined,
  keyDown: () => undefined,
  loadMore: () => undefined,
  moreKeyDown: () => undefined,
};

export function ExplorerTree(props: Props) {
  // One right-click menu replaces the per-row `•••` button that used to appear
  // on hover, and the three header buttons above it. Nothing in this tree is a
  // resting control any more.
  const [menu, setMenu] = useState<{ entry?: FileEntry; anchor: ContextMenuAnchor }>();
  const [pending, setPending] = useState<PendingMutation>();
  // Which *row* has the keyboard, not which position. A precise external
  // change inserts or removes one row without re-listing anything, so a
  // position moved the user's cursor to a different file every time an agent
  // touched the directory they were navigating — and deleting the focused row
  // silently dropped DOM focus to the document body.
  const [focusKey, setFocusKey] = useState<string>();
  const lastFocusIndex = useRef(0);
  // VS Code's escape hatch, and the reason hiding them is safe: the rule is
  // reversible from the tree itself, without a settings trip.
  const [showIgnored, setShowIgnored] = useState(false);
  const treeRef = useRef<HTMLDivElement>(null);
  /**
   * Whether the keyboard is inside this tree.
   *
   * Maintained as focus moves rather than asked for after the fact. A row
   * being unmounted sends focus to `<body>`, which is indistinguishable from
   * "the tree lost focus" if you only look afterwards — and on WebKit and
   * Blink the removal reports no blur at all, so there is nothing to look at.
   */
  const ownsFocus = useRef(false);
  const rootName = props.root?.path.split("/").filter(Boolean).at(-1) ?? props.root?.path ?? "No active root";
  const hidden = showIgnored ? undefined : props.ignoredPaths;
  const rows = useMemo(() => props.root ? flattenTree(props.root.path, props.listings, props.expanded, hidden) : [], [hidden, props.expanded, props.listings, props.root]);
  const heldIndex = focusKey === undefined ? -1 : rows.findIndex((row) => rowKey(row) === focusKey);
  // The row is gone — deleted, collapsed away, filtered out. Focus stays where
  // the user put it rather than jumping to the top, and the effect below hands
  // the element there the real DOM focus.
  const focusIndex = heldIndex >= 0 ? heldIndex : Math.min(lastFocusIndex.current, Math.max(0, rows.length - 1));
  const viewport = useTreeViewport(treeRef, rows.length);
  const mounted = rowWindow({
    rowCount: rows.length,
    rowHeight: viewport.rowHeight,
    scrollTop: viewport.scrollTop,
    viewportHeight: viewport.height,
    focusIndex,
  });
  // The real cost the row budget is about, so the metric stays about mounted
  // rows rather than the logical model. Below the windowing threshold this is
  // every row and the tree behaves exactly as it always has.
  const renderedRowCount = mountedRowCount(mounted);
  useEffect(() => {
    recordPerfHighWater("explorer.logicalRows", rows.length);
    recordPerfHighWater("explorer.renderedRows", renderedRowCount);
    // Counts, not the slice: the slice is a fresh array on every render, and
    // depending on it would run this on every render for no new information.
  }, [renderedRowCount, rows.length]);
  useLayoutEffect(() => { lastFocusIndex.current = focusIndex; }, [focusIndex]);
  useEffect(() => {
    if (heldIndex >= 0 || focusKey === undefined) return;
    const replacement = rows[focusIndex];
    if (!replacement) return;
    // Whether this tree had the keyboard *before* the row went away. Asking
    // the document now cannot answer it: removing a focused element moves
    // focus to `<body>`, so a `contains(document.activeElement)` check here is
    // false in exactly the case it was written for — and the restore never
    // ran. Ownership is therefore tracked as it changes, below.
    const owned = ownsFocus.current;
    setFocusKey(rowKey(replacement));
    if (owned) treeRef.current?.querySelector<HTMLElement>(`[data-tree-index="${focusIndex}"]`)?.focus();
  }, [focusIndex, focusKey, heldIndex, rows]);
  // A new root is a new repository, and the toggle is not offered when that
  // repository has nothing ignored — so a `true` carried across would leave
  // ignored files showing with no visible reason and no way to put them back.
  useEffect(() => { setPending(undefined); setShowIgnored(false); }, [props.root?.token, props.scopeIdentity]);

  // One stable object for every row, forwarding to handlers a layout effect
  // keeps current. Rebuilding the object on each render would defeat the row
  // memo boundary entirely — the props would differ every time even when the
  // row did not — and assigning the live handlers during render would publish
  // closures over state a discarded render never committed.
  const liveRowActions = useRef<ExplorerRowActions>(INERT_ROW_ACTIONS);
  const rowActionsRef = useMemo<ExplorerRowActions>(() => ({
    toggle: (path) => liveRowActions.current.toggle(path),
    open: (entry, options) => liveRowActions.current.open(entry, options),
    focus: (index) => liveRowActions.current.focus(index),
    contextMenu: (entry, anchor, index) => liveRowActions.current.contextMenu(entry, anchor, index),
    keyDown: (event, index, depth, entry) => liveRowActions.current.keyDown(event, index, depth, entry),
    loadMore: (directory) => liveRowActions.current.loadMore(directory),
    moreKeyDown: (event, index) => liveRowActions.current.moreKeyDown(event, index),
  }), []);

  const focusRow = (index: number) => {
    const next = Math.max(0, Math.min(rows.length - 1, index));
    const row = rows[next];
    if (!row) return;
    ownsFocus.current = true;
    setFocusKey(rowKey(row));
    lastFocusIndex.current = next;
    // Bring the row into view before asking for focus. The window always keeps
    // the focused row mounted, so this is about what the user can see rather
    // than about whether the element exists.
    const offset = scrollOffsetForRow({
      index: next,
      rowHeight: viewport.rowHeight,
      scrollTop: viewport.scrollTop,
      viewportHeight: viewport.height,
    });
    if (offset !== undefined && treeRef.current) {
      treeRef.current.scrollTop = offset;
      viewport.setScrollTop(offset);
    }
    globalThis.requestAnimationFrame(() => treeRef.current?.querySelector<HTMLElement>(`[data-tree-index="${next}"]`)?.focus());
  };

  const navigateEntry = (event: KeyboardEvent<HTMLElement>, index: number, depth: number, entry: FileEntry) => {
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

  const committedRowActions: ExplorerRowActions = {
    toggle: (path) => props.onToggle(path),
    open: (entry, options) => props.onOpen(entry, options),
    focus: (index) => {
      const row = rows[index];
      if (!row) return;
      // A row reporting focus *is* the tree owning the keyboard. Recorded here
      // rather than only from the container's own focus event, because that is
      // the fact, and because it does not depend on an event reaching an
      // ancestor.
      ownsFocus.current = true;
      setFocusKey(rowKey(row));
      lastFocusIndex.current = index;
    },
    contextMenu: (entry, anchor, index) => {
      focusRow(index);
      setMenu({ ...(entry ? { entry } : {}), anchor });
    },
    keyDown: (event, index, depth, entry) => navigateEntry(event, index, depth, entry),
    loadMore: (directory) => props.onLoadMore(directory),
    moreKeyDown: (event, index) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); focusRow(index + (event.key === "ArrowDown" ? 1 : -1)); }
    },
  };
  useLayoutEffect(() => { liveRowActions.current = committedRowActions; });

  const begin = (action: PendingMutation["action"], entry?: FileEntry) => {
    if (!props.root) return;
    setPending({ action, entry, rootToken: props.root.token, scopeIdentity: props.scopeIdentity });
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
  const committedRowCommand = (commandId: CommandId) => {
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
  useLayoutEffect(() => { runRowCommand.current = committedRowCommand; });
  const rowSource = useMemo<RowCommandSource | undefined>(() => rowActions.length === 0 ? undefined : {
    subject: focusedEntry?.name ?? rootName,
    available: rowActions,
    run: (commandId) => runRowCommand.current(commandId),
  }, [focusedEntry, rootName, rowActions]);
  usePublishedRowCommands("files", rowSource);

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
      onBlur={(event) => {
        // Two things look alike from here and are not. A row unmounted under
        // the cursor reports no `relatedTarget` — and the row it left is
        // already detached — which is the one case the restore above exists
        // for. A click on non-focusable background also reports no
        // `relatedTarget`, but the row it left is still in the document, and
        // that genuinely does give the keyboard away: treating it as a removal
        // would let the next external change pull focus back into a tree the
        // user had put it down in.
        const gaveItAway = event.relatedTarget
          ? !event.currentTarget.contains(event.relatedTarget)
          : event.target.isConnected;
        if (gaveItAway) ownsFocus.current = false;
      }}
      onFocus={() => { ownsFocus.current = true; }}
      onScroll={(event) => viewport.observeScroll(event.currentTarget.scrollTop)}
      ref={treeRef}
      role="tree"
    >
      {/* Reserved height stands in for the rows that are not mounted, so the
          scrollbar describes the whole directory rather than the slice. All of
          it is zero below the windowing threshold. */}
      {mounted.segments.map((segment) => <Fragment key={`segment:${segment.start}`}>
        {segment.leadingHeight > 0 && <div aria-hidden="true" style={{ height: `${segment.leadingHeight}px` }} />}
        {rows.slice(segment.start, segment.end).map((row, offset) => {
          const index = segment.start + offset;
          return row.kind === "more"
            ? <ExplorerMoreRow
              actions={rowActionsRef}
              depth={row.depth}
              directory={row.directory}
              disabled={props.loading.has(row.directory)}
              focused={index === focusIndex}
              index={index}
              key={`more:${row.directory}`}
              positionInSet={row.positionInSet}
              setSize={row.setSize}
            />
            : <ExplorerEntryRow
              actions={rowActionsRef}
              depth={row.depth}
              entry={row.entry}
              focused={index === focusIndex}
              index={index}
              key={row.entry.path}
              open={props.expanded.has(row.entry.path)}
              positionInSet={row.positionInSet}
              setSize={row.setSize}
            />;
        })}
      </Fragment>)}
      {mounted.trailingHeight > 0 && <div aria-hidden="true" style={{ height: `${mounted.trailingHeight}px` }} />}
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
          { id: "move", label: "Move…", disabled: props.disabled, run: () => begin("move", menu.entry) },
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
    {pending && <ExplorerMutationDialog
      disabled={props.disabled}
      onClose={() => setPending(undefined)}
      onMutate={props.onMutate}
      pending={pending}
      root={props.root}
      scopeIdentity={props.scopeIdentity}
    />}
  </div>;
}

function flattenTree(
  root: string,
  listings: ReadonlyMap<string, DirectoryListing>,
  expanded: ReadonlySet<string>,
  ignored?: ReadonlySet<string>,
) {
  const rows: ExplorerRowModel[] = [];
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
    const shown = (listing?.entries ?? []).filter((entry) => !ignored?.has(entry.path));
    // Sibling counts, because windowing means an assistive technology can no
    // longer infer position from what happens to be in the DOM. They are per
    // level, as the tree role requires — not positions in the flattened walk.
    const siblings = shown.length + (listing && !listing.complete && listing.nextPageToken ? 1 : 0);
    let position = 0;
    for (const entry of shown) {
      position += 1;
      rows.push({ kind: "entry", entry, depth, positionInSet: position, setSize: siblings });
      if (entry.expandable && expanded.has(entry.path)) visit(entry.path, depth + 1);
    }
    if (listing && !listing.complete && listing.nextPageToken) {
      rows.push({ kind: "more", directory, depth, positionInSet: siblings, setSize: siblings });
    }
  };
  visit(root, 0);
  return rows;
}

/** One row's stable identity, which is what the keyboard actually holds. */
function rowKey(row: ExplorerRowModel): string {
  return row.kind === "entry" ? row.entry.path : `${row.directory}\u0000more`;
}

type ExplorerRowModel =
  | { kind: "entry"; entry: FileEntry; depth: number; positionInSet: number; setSize: number }
  | { kind: "more"; directory: string; depth: number; positionInSet: number; setSize: number };

/**
 * The tree's scroll geometry, measured rather than assumed.
 *
 * Row height comes from a real mounted row so the reserved spacer heights match
 * what the stylesheet actually produces; a layout that has not reported one yet
 * falls back to the module default, which only affects the size of the mounted
 * band and never which rows exist.
 */
function useTreeViewport(ref: RefObject<HTMLDivElement | null>, rowCount: number) {
  const [scrollTop, setScrollTop] = useState(0);
  const scrollFrame = useRef(0);
  const [height, setHeight] = useState(0);
  const [rowHeight, setRowHeight] = useState(DEFAULT_ROW_HEIGHT);
  // Layout effects, not passive ones: they run before the browser paints, so
  // the first frame the user actually sees is already sized by the real
  // viewport rather than by the pre-layout assumption.
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    // Measured directly, and first. Leaving this to `ResizeObserver` alone left
    // the viewport at zero — and therefore assumed — on the first commit, and
    // permanently wherever that observer does not exist, so a tree taller than
    // the assumption rendered blank space below its band.
    setHeight(node.clientHeight);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => setHeight(node.clientHeight));
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref, rowCount]);
  useLayoutEffect(() => {
    const measured = ref.current?.querySelector<HTMLElement>(".file-row")?.offsetHeight ?? 0;
    if (measured > 0) setRowHeight((current) => (current === measured ? current : measured));
  }, [ref, rowCount]);
  // A shorter tree can leave the viewport scrolled past its own content.
  useEffect(() => { if (rowCount === 0) setScrollTop(0); }, [rowCount]);
  useEffect(() => () => globalThis.cancelAnimationFrame?.(scrollFrame.current), []);
  return {
    height,
    rowHeight,
    scrollTop,
    setScrollTop,
    /**
     * Coalesces scrolling to one commit per frame.
     *
     * A scroll event per state commit re-slices and re-renders the mounted
     * band, which on the surface windowing exists to keep under a frame budget
     * is the one place that cannot afford a render per event.
     */
    observeScroll: (offset: number) => {
      globalThis.cancelAnimationFrame?.(scrollFrame.current);
      scrollFrame.current = globalThis.requestAnimationFrame(() => setScrollTop(offset));
    },
  };
}


import { memo, type KeyboardEvent } from "react";
import type { ContextMenuAnchor } from "../../ui/ContextMenu";
import { Icon } from "../../ui/Icon";
import { fileIcon } from "./fileIcons";
import type { FileEntry } from "./types";

/**
 * The row's whole interface to the tree, deliberately a stable object.
 *
 * Every handler a row used to receive closed over render-fresh state, so one
 * focus move or one keystroke in the rename dialog rebuilt every handler of
 * every row and re-rendered all of them. At 4,096 rows that is the difference
 * between two rows repainting and the whole tree doing it.
 */
export interface ExplorerRowActions {
  toggle(path: string): void;
  open(entry: FileEntry, options: { preview: boolean }): void;
  focus(index: number): void;
  contextMenu(entry: FileEntry | undefined, anchor: ContextMenuAnchor, index: number): void;
  keyDown(event: KeyboardEvent<HTMLElement>, index: number, depth: number, entry: FileEntry): void;
  loadMore(directory: string): void;
  moreKeyDown(event: KeyboardEvent<HTMLElement>, index: number): void;
  drag(entry: FileEntry, transfer: DataTransfer): void;
  dragEnd(accepted: boolean): void;
}

interface EntryRowProps {
  actions: ExplorerRowActions;
  depth: number;
  dragEnabled: boolean;
  entry: FileEntry;
  focused: boolean;
  index: number;
  open: boolean;
  /** Position among siblings at this level, which windowing hides from the DOM. */
  positionInSet: number;
  setSize: number;
}

export const ExplorerEntryRow = memo(function ExplorerEntryRow(props: EntryRowProps) {
  const { actions, depth, entry, focused, index, open } = props;
  const icon = fileIcon(entry, open);
  return <div
    aria-expanded={entry.expandable ? open : undefined}
    aria-level={depth + 1}
    aria-posinset={props.positionInSet}
    aria-selected={focused}
    aria-setsize={props.setSize}
    className="file-row"
    data-tree-index={index}
    draggable={props.dragEnabled}
    onClick={(event) => { if (event.target === event.currentTarget) entry.expandable ? actions.toggle(entry.path) : actions.open(entry, { preview: true }); }}
    onContextMenu={(event) => {
      event.preventDefault();
      actions.contextMenu(entry, { x: event.clientX, y: event.clientY }, index);
    }}
    onDoubleClick={(event) => {
      // The row's indent strip is outside the button but inside the row, so
      // without this a file reached by clicking its padding could be previewed
      // forever and never pinned.
      if (event.target === event.currentTarget && !entry.expandable) actions.open(entry, { preview: false });
    }}
    onFocus={() => actions.focus(index)}
    onDragStart={(event) => { if (props.dragEnabled) actions.drag(entry, event.dataTransfer); }}
    onDragEnd={(event) => actions.dragEnd(event.dataTransfer.dropEffect !== "none")}
    onKeyDown={(event) => actions.keyDown(event, index, depth, entry)}
    onPointerDown={() => actions.focus(index)}
    role="treeitem"
    style={{ paddingLeft: `${8 + depth * 14}px` }}
    tabIndex={focused ? 0 : -1}
  >
    {/* The click of a double-click fires first and opens the preview; the
        second click then pins that same tab, which is exactly the VS Code
        behaviour and needs no click-delay timer. */}
    <button
      className="file-main"
      onClick={() => entry.expandable ? actions.toggle(entry.path) : actions.open(entry, { preview: true })}
      onDoubleClick={() => { if (!entry.expandable) actions.open(entry, { preview: false }); }}
      tabIndex={-1}
      type="button"
    >
      <span className="file-twisty">{entry.expandable ? <Icon name={open ? "chevronDown" : "chevronRight"} size={11} /> : null}</span>
      <span className={`file-icon ${entry.kind}`} style={{ color: icon.color }}><Icon name={icon.icon} size={14} /></span>
      <span title={entryTooltip(entry)}>{entry.name}</span>
    </button>
  </div>;
});

interface MoreRowProps {
  actions: ExplorerRowActions;
  depth: number;
  directory: string;
  disabled: boolean;
  focused: boolean;
  index: number;
  positionInSet: number;
  setSize: number;
}

export const ExplorerMoreRow = memo(function ExplorerMoreRow(props: MoreRowProps) {
  const { actions, depth, directory, disabled, focused, index } = props;
  return <button
    // `aria-disabled`, never `disabled`. A disabled button is removed from the
    // tab order and cannot hold DOM focus, so pressing this row — which is
    // what sets `loading`, which is what disables it — dropped focus to the
    // document body mid-interaction and left arrow-key navigation of the whole
    // tree dead until the user clicked something.
    aria-disabled={disabled || undefined}
    aria-level={depth + 1}
    aria-posinset={props.positionInSet}
    aria-selected={focused}
    aria-setsize={props.setSize}
    className="load-more-files"
    data-tree-index={index}
    onClick={() => { if (!disabled) actions.loadMore(directory); }}
    onFocus={() => actions.focus(index)}
    onKeyDown={(event) => actions.moreKeyDown(event, index)}
    role="treeitem"
    style={{ marginLeft: `${8 + depth * 14}px` }}
    tabIndex={focused ? 0 : -1}
    type="button"
  >Load more…</button>;
});

export function entryTooltip(entry: FileEntry): string {
  const modified = Number(entry.modifiedMillis);
  const lines = [entry.path, `${entry.kind} · ${entry.sizeBytes} bytes`];
  if (Number.isFinite(modified) && modified > 0) lines.push(`Modified ${new Date(modified).toLocaleString()}`);
  if (entry.symlinkTarget) lines.push(`Symlink → ${entry.symlinkTarget}`);
  return lines.join("\n");
}

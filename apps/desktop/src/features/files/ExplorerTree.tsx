import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import type { ActiveRoot, DirectoryListing, DownloadRequest, FileEntry, FileMutation, TransferStatus } from "./types";
import { canCancelTransfer, transferStateLabel } from "../transfers/transferState";

interface Props {
  root?: ActiveRoot;
  listings: ReadonlyMap<string, DirectoryListing>;
  expanded: ReadonlySet<string>;
  loading: ReadonlySet<string>;
  transfers: readonly TransferStatus[];
  scopeIdentity: string;
  disabled: boolean;
  error?: string;
  onToggle(path: string): void;
  onOpen(entry: FileEntry): void;
  onMutate(mutation: FileMutation): Promise<void>;
  onDownload(request: DownloadRequest): Promise<void>;
  onCancelTransfer(id: string): Promise<void>;
  onRefresh(path?: string): void;
  onLoadMore(path: string): void;
}

type PendingAction = { action: "newFile" | "newDirectory" | "rename" | "move" | "duplicate" | "delete"; rootToken: string; scopeIdentity: string; entry?: FileEntry };

export function ExplorerTree(props: Props) {
  const [menu, setMenu] = useState<string>();
  const [pending, setPending] = useState<PendingAction>();
  const [value, setValue] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const [nonEmptyOverwrite, setNonEmptyOverwrite] = useState(false);
  const [dialogError, setDialogError] = useState<string>();
  const [focusIndex, setFocusIndex] = useState(0);
  const treeRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const composing = useRef(false);
  const dialogTitleId = useId();
  const closeDialog = () => setPending(undefined);
  const dialogRef = useModalDialog<HTMLFormElement>(closeDialog, Boolean(pending));
  const rootName = props.root?.path.split("/").filter(Boolean).at(-1) ?? props.root?.path ?? "No active root";
  const rows = useMemo(() => props.root ? flattenTree(props.root.path, props.listings, props.expanded) : [], [props.expanded, props.listings, props.root]);
  useEffect(() => setFocusIndex((current) => Math.min(current, Math.max(0, rows.length - 1))), [rows.length]);
  useEffect(() => setPending(undefined), [props.root?.token, props.scopeIdentity]);
  useEffect(() => {
    if (menu) menuRef.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }, [menu]);

  const focusRow = (index: number) => {
    const next = Math.max(0, Math.min(rows.length - 1, index));
    setFocusIndex(next);
    window.requestAnimationFrame(() => treeRef.current?.querySelector<HTMLElement>(`[data-tree-index="${next}"]`)?.focus());
  };

  const navigateEntry = (event: KeyboardEvent<HTMLDivElement>, index: number, depth: number, entry: FileEntry) => {
    if (event.target !== event.currentTarget) return;
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
      if (entry.expandable) props.onToggle(entry.path); else props.onOpen(entry);
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

  return <div className="explorer-tree">
    <header className="explorer-root">
      <span title={props.root?.path}>{rootName}</span>
      {props.root && <small>{props.root.gitWorktree ? "Git worktree" : "pane CWD"}</small>}
      <div>
        <button aria-label="New file" disabled={props.disabled || !props.root} onClick={() => begin("newFile")} type="button">＋F</button>
        <button aria-label="New folder" disabled={props.disabled || !props.root} onClick={() => begin("newDirectory")} type="button">＋D</button>
        <button aria-label="Refresh Explorer" onClick={() => props.onRefresh()} type="button">↻</button>
      </div>
    </header>
    {props.error && <div className="explorer-error" role="alert">{props.error}</div>}
    <div aria-label="Files" className="file-tree" ref={treeRef} role="tree">
      {rows.map((row, index) => {
        if (row.kind === "more") return <button aria-level={row.depth + 1} className="load-more-files" data-tree-index={index} disabled={props.loading.has(row.directory)} key={`more:${row.directory}`} onClick={() => props.onLoadMore(row.directory)} onFocus={() => setFocusIndex(index)} onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); focusRow(index + (event.key === "ArrowDown" ? 1 : -1)); }
        }} role="treeitem" style={{ marginLeft: `${8 + row.depth * 14}px` }} tabIndex={index === focusIndex ? 0 : -1} type="button">Load more…</button>;
        const { entry, depth } = row;
        const isDirectory = entry.kind === "directory";
        const isOpen = props.expanded.has(entry.path);
        return <div aria-expanded={entry.expandable ? isOpen : undefined} aria-level={depth + 1} aria-selected={index === focusIndex} className="file-row" data-tree-index={index} key={entry.path} onClick={(event) => { if (event.target === event.currentTarget) entry.expandable ? props.onToggle(entry.path) : props.onOpen(entry); }} onFocus={() => setFocusIndex(index)} onKeyDown={(event) => navigateEntry(event, index, depth, entry)} role="treeitem" style={{ paddingLeft: `${8 + depth * 14}px` }} tabIndex={index === focusIndex ? 0 : -1}>
          <button className="file-main" onClick={() => entry.expandable ? props.onToggle(entry.path) : props.onOpen(entry)} tabIndex={-1} type="button">
            <span aria-hidden="true">{entry.expandable ? (isOpen ? "⌄" : "›") : "·"}</span>
            <span aria-hidden="true">{isDirectory ? "▱" : entry.kind === "symlink" ? "↗" : "▧"}</span>
            <span title={entryTooltip(entry)}>{entry.name}</span>
          </button>
          <button aria-label={`Actions for ${entry.name}`} className="file-actions" onClick={() => setMenu(menu === entry.path ? undefined : entry.path)} type="button">•••</button>
          {menu === entry.path && <div className="file-menu" onKeyDown={(event) => {
            const items = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
            const current = items.indexOf(document.activeElement as HTMLButtonElement);
            let next: number | undefined;
            if (event.key === "ArrowDown") next = (current + 1) % items.length;
            else if (event.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
            else if (event.key === "Home") next = 0;
            else if (event.key === "End") next = items.length - 1;
            else if (event.key === "Escape") { event.preventDefault(); setMenu(undefined); return; }
            if (next !== undefined) { event.preventDefault(); items[next]?.focus(); }
          }} ref={menuRef} role="menu">
            {!isDirectory && <button onClick={() => props.onOpen(entry)} role="menuitem" type="button">Open</button>}
            <button disabled={props.disabled} onClick={() => begin("rename", entry)} role="menuitem" type="button">Rename…</button>
            <button disabled={props.disabled} onClick={() => { setValue(""); begin("move", entry); }} role="menuitem" type="button">Move…</button>
            <button disabled={props.disabled} onClick={() => begin("duplicate", entry)} role="menuitem" type="button">Duplicate…</button>
            <button onClick={() => void props.onDownload({ path: entry.path, kind: isDirectory ? "folder" : "file", collision: "fail" })} role="menuitem" type="button">Download{isDirectory ? " Folder" : ""}…</button>
            <button className="danger-text" disabled={props.disabled} onClick={() => begin("delete", entry)} role="menuitem" type="button">Delete…</button>
          </div>}
        </div>;
      })}
      {props.root && props.loading.has(props.root.path) && <div className="tree-loading" role="status">Loading…</div>}
      {!props.root && <div className="tree-empty">Select a live terminal pane.</div>}
    </div>
    {props.transfers.length > 0 && <section aria-label="Downloads" className="transfers">
      <h3>Downloads</h3>
      {props.transfers.map((transfer) => <div aria-label={`Download ${transfer.path}: ${transferStateLabel(transfer.state)}`} className={`transfer ${transfer.state}`} key={transfer.id}>
        <span>{transfer.path.split("/").at(-1)}</span><small>{transferStateLabel(transfer.state)}</small>
        {transfer.totalBytes ? <progress aria-label={`Download progress for ${transfer.path}`} aria-valuetext={formatTransfer(transfer)} data-completed-bytes={transfer.completedBytes} data-total-bytes={transfer.totalBytes} max={1000} value={transferPermille(transfer.completedBytes, transfer.totalBytes)} /> : <progress aria-label={`Download progress for ${transfer.path}`} data-completed-bytes={transfer.completedBytes} />}
        <small className="transfer-detail">{formatTransfer(transfer)}</small>
        {canCancelTransfer(transfer.state) && <button aria-label={`Cancel download ${transfer.path}`} onClick={() => void props.onCancelTransfer(transfer.id)} type="button">Cancel</button>}
        {transfer.state === "verifying" && <small className="transfer-detail transfer-finalizing" role="status">The verified bytes are being committed; awaiting the authoritative backend outcome.</small>}
        {transfer.failureKind === "staleScope" && <em role="alert">Download stopped because the connection scope changed.</em>}
        {transfer.failureKind === "timeout" && <em role="alert">Download timed out before an authoritative result arrived.</em>}
        {transfer.outcome === "unknown" && <em role="alert">The download outcome is unknown. Inspect the destination before retrying.</em>}
        {transfer.error && <em role="alert">{transfer.error}</em>}
        {transfer.cleanupError && <em role="alert">Partial cleanup failed: {transfer.cleanupError}</em>}
        {transfer.cleanupStatus && ["failed", "cancelled"].includes(transfer.state) && <small className="transfer-detail">Cleanup: {transfer.cleanupStatus}</small>}
      </div>)}
    </section>}
    {pending && <div className="modal-backdrop" role="presentation"><form aria-labelledby={dialogTitleId} aria-modal="true" className="file-dialog confirmation" onSubmit={(event) => { event.preventDefault(); if (!composing.current) void submit(); }} ref={dialogRef} role="dialog">
      <h2 id={dialogTitleId}>{labelForAction(pending.action)}</h2>
      {pending.action === "delete" ? <p>Delete <code>{pending.entry?.path}</code>? {pending.entry?.kind === "directory" && "Non-empty directories require this confirmation."}</p> : <label>
        {pending.action.startsWith("new") ? "Name" : "Destination path"}
        <input autoFocus onChange={(event) => setValue(event.target.value)} onCompositionEnd={() => { composing.current = false; }} onCompositionStart={() => { composing.current = true; }} value={value} />
      </label>}
      {["rename", "move", "duplicate"].includes(pending.action) && <label className="overwrite"><input checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} type="checkbox" /> Allow overwrite after confirmation</label>}
      {["rename", "move", "duplicate"].includes(pending.action) && overwrite && <label className="overwrite"><input checked={nonEmptyOverwrite} onChange={(event) => setNonEmptyOverwrite(event.target.checked)} type="checkbox" /> Also replace a non-empty destination directory</label>}
      {dialogError && <div className="explorer-error" role="alert">{dialogError}</div>}
      <div className="dialog-actions"><button onClick={() => setPending(undefined)} type="button">Cancel</button><button className={pending.action === "delete" ? "danger" : "primary"} type="submit">{pending.action === "delete" ? "Delete" : "Apply"}</button></div>
    </form></div>}
  </div>;
}

function flattenTree(root: string, listings: ReadonlyMap<string, DirectoryListing>, expanded: ReadonlySet<string>) {
  const rows: ({ kind: "entry"; entry: FileEntry; depth: number } | { kind: "more"; directory: string; depth: number })[] = [];
  const visit = (directory: string, depth: number) => {
    const listing = listings.get(directory);
    for (const entry of listing?.entries ?? []) {
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

function formatTransfer(transfer: TransferStatus): string {
  const progress = transfer.totalBytes
    ? `${formatTransferBytes(transfer.completedBytes)} / ${formatTransferBytes(transfer.totalBytes)}`
    : `${formatTransferBytes(transfer.completedBytes)} transferred`;
  const speed = transfer.bytesPerSecond ? ` · ${formatTransferBytes(transfer.bytesPerSecond)}/s` : "";
  const eta = transfer.etaSeconds !== undefined && transfer.etaSeconds > 0 ? ` · ${Math.ceil(transfer.etaSeconds)}s remaining` : "";
  return `${progress}${speed}${eta}`;
}

function formatTransferBytes(value: string): string {
  if (!/^(0|[1-9]\d*)$/.test(value)) return `${value} B`;
  const bytes = BigInt(value);
  const units = [[1024n ** 4n, "TiB"], [1024n ** 3n, "GiB"], [1024n ** 2n, "MiB"], [1024n, "KiB"]] as const;
  for (const [size, label] of units) {
    if (bytes >= size) {
      const tenths = bytes * 10n / size;
      return `${tenths / 10n}.${tenths % 10n} ${label}`;
    }
  }
  return `${bytes} B`;
}

function transferPermille(completed: string, total: string): number {
  const numerator = BigInt(completed);
  const denominator = BigInt(total);
  if (denominator <= 0n) return 0;
  return Number((numerator > denominator ? denominator : numerator) * 1000n / denominator);
}

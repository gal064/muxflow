import { useEffect, useId, useMemo, useRef, useState } from "react";
import { keyboardEventIsComposing } from "../../commands/registry";
import { fuzzyMatch } from "../../commands/fuzzy";
import { useModalDialog } from "../../commands/useModalDialog";
import { AgentStateIndicator } from "../../ui/AgentStateIndicator";
import { Icon } from "../../ui/Icon";
import { fileIcon } from "../files/fileIcons";
import type { FileSearchClient, FileWorkspaceScope, TerminalFilePaneRoute } from "../files/types";
import { selectableTabs, type CombinedTab, type SelectableTab } from "../shell/model";
import type { MergedWorkspaceRow } from "./mergedWorkspaceRows";
import { quickOpenItems, type QuickOpenItem } from "./quickOpen";
import { useQuickOpenSearch } from "./useQuickOpenSearch";

interface WorkspaceSwitcherProps {
  rows: readonly MergedWorkspaceRow[];
  tabs: readonly CombinedTab[];
  activeTabKey?: string;
  client: FileSearchClient;
  scope?: FileWorkspaceScope;
  pane?: TerminalFilePaneRoute;
  hostLabel: string;
  hostLabelFor(id: string): string;
  stateGlyphs: boolean;
  onClose(): void;
  onSelect(row: MergedWorkspaceRow): void;
  onSelectTab(tab: SelectableTab): void;
  onOpenFile(path: string): void;
}

function Highlight({ text, query }: { text: string; query: string }) {
  const indices = new Set(fuzzyMatch(text, query)?.indices ?? []);
  // Match indices use JS string offsets, including surrogate pairs.
  let offset = 0;
  return <>{Array.from(text).map((character) => {
    const position = offset;
    offset += character.length;
    return indices.has(position) ? <mark key={position}>{character}</mark> : character;
  })}</>;
}

/** The existing ⌘P command now opens files, workspaces, and current-workspace tabs. */
export function WorkspaceSwitcher(props: WorkspaceSwitcherProps) {
  const [query, setQuery] = useState("");
  const normalized = query.trim();
  const [selection, setSelection] = useState<{ query: string; key?: string }>({ query: "" });
  const titleId = useId();
  const resultsId = useId();
  const dialog = useModalDialog<HTMLElement>(props.onClose);
  const listRef = useRef<HTMLDivElement>(null);
  const search = useQuickOpenSearch(props.client, props.scope, props.pane, normalized);
  const items = useMemo(() => quickOpenItems(
    props.rows, selectableTabs(props.tabs), search.matches, normalized, props.activeTabKey, props.hostLabelFor,
    selection.query === normalized ? selection.key : undefined,
  ), [props.rows, props.tabs, search.matches, normalized, props.activeTabKey, props.hostLabelFor, selection]);
  const active = (selection.query === normalized ? items.find((item) => item.key === selection.key) : undefined) ?? items[0];
  const activeKey = active?.key;
  const optionId = (key: string) => `${resultsId}-${encodeURIComponent(key)}`;

  useEffect(() => {
    setSelection((current) => current.query === normalized && current.key === activeKey ? current : { query: normalized, key: activeKey });
    listRef.current?.querySelector<HTMLElement>("[aria-selected=true]")?.scrollIntoView({ block: "nearest" });
  }, [activeKey, normalized, items]);

  const choose = (item: QuickOpenItem | undefined) => {
    if (!item) return;
    if (item.kind === "workspace") props.onSelect(item.workspace);
    else if (item.kind === "tab") props.onSelectTab(item.tab);
    else props.onOpenFile(item.file.path);
    props.onClose();
  };

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) props.onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="palette quick-open" ref={dialog} role="dialog">
      <h2 className="sr-only" id={titleId}>Quick Open</h2>
      <div className="quick-open-input">
        <div className="quick-open-field">
          <Icon name="search" size={14} />
          <input
            aria-activedescendant={active ? optionId(active.key) : undefined}
            aria-controls={resultsId}
            aria-expanded="true"
            aria-label="Search files, workspaces, and tabs"
            role="combobox"
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (keyboardEventIsComposing(event.nativeEvent)) return;
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                const index = items.findIndex((item) => item.key === activeKey);
                const next = items[Math.max(0, Math.min(items.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)))];
                setSelection({ query: normalized, key: next?.key });
              } else if (event.key === "Enter") { event.preventDefault(); choose(active); }
            }}
            placeholder="Search files, workspaces, and tabs…"
            maxLength={256}
            value={query}
          />
        </div>
      </div>
      <div className="quick-open-heading" aria-live="polite">
        <span>{normalized ? "Matches" : "Open tabs & workspaces"}</span>
        <span>{search.loading ? "Searching files…" : `${items.length} results`}</span>
      </div>
      <div className="palette-list quick-open-list" id={resultsId} ref={listRef} role="listbox">
        {items.length === 0 && <p className="quiet-empty">{search.loading ? "Searching filenames…" : "No matching names or paths."}</p>}
        {items.map((item) => {
          const file = item.kind === "file" || (item.kind === "tab" && item.tab.kind === "app" && ["file", "markdown"].includes(item.tab.appKind));
          const icon = file ? fileIcon({ name: item.title, kind: "file" }) : undefined;
          return <button
            aria-selected={item.key === activeKey}
            className={item.key === activeKey ? "palette-row quick-open-row selected" : "palette-row quick-open-row"}
            id={optionId(item.key)} key={item.key}
            onClick={() => choose(item)}
            onMouseMove={() => setSelection((current) => current.query === normalized && current.key === item.key ? current : { query: normalized, key: item.key })}
            onMouseDown={(event) => event.preventDefault()}
            role="option" tabIndex={-1} type="button"
          >
            <span className={`quick-open-icon ${item.kind}`} style={icon ? { color: icon.color } : undefined}>
              <Icon name={icon?.icon ?? (item.kind === "workspace" ? "sidebarLeft" : "fileShell")} size={16} />
            </span>
            {item.kind === "workspace" && item.workspace.letter && <span className="host-letter">{item.workspace.letter}</span>}
            <span className="palette-title quick-open-title"><Highlight text={item.title} query={normalized} /></span>
            <span className="quick-open-detail" title={item.detail}><Highlight text={item.detail} query={normalized} /></span>
            {item.kind === "workspace" && item.workspace.attention !== "none" && <AgentStateIndicator glyphs={props.stateGlyphs} label={`Agent ${item.workspace.attention}`} state={item.workspace.attention} />}
            <span className="quick-open-kind">{item.kind === "file" ? "File" : item.kind === "workspace" ? "Workspace" : file ? "Open tab" : "Tab"}{item.current ? " · current" : ""}</span>
          </button>;
        })}
      </div>
      {(search.error || !search.complete) && <p className="quick-open-status" role="status">
        {search.error ? "File search unavailable. Workspaces and tabs are still searchable." : "Search limited—narrow your query."}
      </p>}
      <div className="quick-open-scope">
        <Icon name="folder" size={12} />
        <span title={props.pane?.cwd}>{props.pane?.cwd ?? "No focused terminal directory"}</span>
        <small title={props.hostLabel}>{props.hostLabel}<span className="quick-open-pane-label"> · focused pane</span></small>
      </div>
      <div className="quick-open-footer" aria-hidden="true">
        <span>↑ ↓ navigate</span><span>↵ open / switch</span><span>esc close</span><span>Names & paths</span>
      </div>
    </section>
  </div>;
}

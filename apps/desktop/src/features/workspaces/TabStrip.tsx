import { useState, type KeyboardEvent } from "react";
import { Icon, type IconName } from "../../ui/Icon";
import { ContextMenu, type ContextMenuAnchor } from "../../ui/ContextMenu";
import type { CombinedTab } from "../shell/model";

interface TabStripProps {
  tabs: readonly CombinedTab[];
  activeKey?: string;
  canMutate: boolean;
  canSplit: boolean;
  onSelect(tab: CombinedTab): void;
  onClose(tab: CombinedTab): void;
  onMove(tab: CombinedTab, direction: "left" | "right"): void;
  onRenameTerminal(tab: Extract<CombinedTab, { kind: "terminal" }>): void;
  onNewTerminal(): void;
  onSplit(): void;
}

const DOCUMENT_ICON: Record<string, IconName> = {
  gitDiff: "diff",
  markdown: "markdown",
  file: "file",
};

/**
 * One strip, two kinds of tab: tmux windows and app-owned documents.
 *
 * Two things about it are load-bearing. First, selecting a tab must not change
 * any tab's width — the previous strip injected four inline buttons into
 * whichever tab was active, so every tab switch reflowed the whole strip and
 * the tab under the pointer moved out from under it. Nothing here appears or
 * disappears on selection.
 *
 * Second, a terminal tab has no close button. Closing a tmux window destroys
 * live processes and goes through the confirmation contract; it lives on the
 * context menu and ⌘W. Document tabs close in place, because closing one throws
 * nothing away.
 */
export function TabStrip(props: TabStripProps) {
  const [menu, setMenu] = useState<{ tab: CombinedTab; anchor: ContextMenuAnchor }>();

  const focusRelative = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let next = index;
    if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowRight") next = Math.min(props.tabs.length - 1, index + 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = props.tabs.length - 1;
    else if (event.key === "Enter" || event.key === " ") return props.onSelect(props.tabs[index]);
    else return;
    event.preventDefault();
    event.currentTarget.closest("[role=tablist]")?.querySelectorAll<HTMLButtonElement>("[role=tab]")[next]?.focus();
  };

  return <div className="tabstrip">
    <div aria-label="Terminal tabs and documents" className="tabstrip-tabs" role="tablist">
      {props.tabs.map((tab, index) => {
        const active = tab.key === props.activeKey;
        return <div className={active ? "tab active" : "tab"} key={tab.key}>
          <button
            aria-controls={workspaceTabPanelDomId(tab.key)}
            aria-selected={active}
            className="tab-select"
            id={workspaceTabDomId(tab.key)}
            onAuxClick={(event) => {
              // Middle-click closes, the way every tabbed app does. Terminal
              // tabs still route through the confirmation contract.
              if (event.button === 1) { event.preventDefault(); props.onClose(tab); }
            }}
            onClick={() => props.onSelect(tab)}
            onContextMenu={(event) => {
              event.preventDefault();
              props.onSelect(tab);
              setMenu({ tab, anchor: { x: event.clientX, y: event.clientY } });
            }}
            onDoubleClick={() => { if (tab.kind === "terminal" && props.canMutate) props.onRenameTerminal(tab); }}
            onKeyDown={(event) => focusRelative(event, index)}
            role="tab"
            tabIndex={active ? 0 : -1}
            title={tab.kind === "app" ? tab.resource : `tmux window ${tab.id}`}
            type="button"
          >
            {tab.kind === "terminal"
              ? <span className="tab-index">{tab.index}</span>
              : <span className={`tab-glyph ${tab.appKind}`}><Icon name={DOCUMENT_ICON[tab.appKind] ?? "file"} size={12} /></span>}
            <span className="tab-title">{tab.title}</span>
            {tab.kind === "terminal" && tab.zoomed && <span aria-label="Pane zoomed" className="tab-zoom"><Icon name="zoom" size={11} /></span>}
            {tab.kind === "terminal" && tab.attention !== "none" && <span
              aria-label={`Agent ${tab.attention}`}
              className={`tab-dot ${tab.attention}`}
              role="img"
            />}
          </button>
          {tab.kind === "app" && <button
            aria-label={`Close ${tab.title}`}
            className="tab-close"
            onClick={() => props.onClose(tab)}
            type="button"
          ><Icon name="close" size={11} /></button>}
        </div>;
      })}
    </div>
    <div className="tabstrip-tools">
      <button aria-label="Split pane right" className="bar-button" disabled={!props.canSplit} onClick={props.onSplit} type="button"><Icon name="splitRight" /></button>
      <button aria-label="New terminal tab" className="bar-button" disabled={!props.canMutate} onClick={props.onNewTerminal} type="button"><Icon name="plus" /></button>
    </div>
    {menu && <ContextMenu
      anchor={menu.anchor}
      items={[
        ...(menu.tab.kind === "terminal"
          ? [{ id: "rename", label: "Rename tab…", disabled: !props.canMutate, run: () => props.onRenameTerminal(menu.tab as Extract<CombinedTab, { kind: "terminal" }>) }]
          : []),
        { id: "left", label: "Move left", disabled: !menu.tab.canMoveLeft || (menu.tab.kind === "terminal" && !props.canMutate), run: () => props.onMove(menu.tab, "left") },
        { id: "right", label: "Move right", disabled: !menu.tab.canMoveRight || (menu.tab.kind === "terminal" && !props.canMutate), run: () => props.onMove(menu.tab, "right") },
        "separator",
        {
          id: "close",
          label: menu.tab.kind === "terminal" ? "Close tab…" : "Close tab",
          destructive: menu.tab.kind === "terminal",
          disabled: menu.tab.kind === "terminal" && !props.canMutate,
          run: () => props.onClose(menu.tab),
        },
      ]}
      label={`Actions for ${menu.tab.title}`}
      onClose={() => setMenu(undefined)}
    />}
  </div>;
}

function encodeDomId(value: string): string {
  return [...value].map((character) => character.codePointAt(0)?.toString(16)).join("-");
}

export function workspaceTabDomId(key: string): string {
  return `workspace-tab-${encodeDomId(key)}`;
}

export function workspaceTabPanelDomId(key: string): string {
  return `workspace-panel-${encodeDomId(key)}`;
}

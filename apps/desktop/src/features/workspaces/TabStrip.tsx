import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { Icon } from "../../ui/Icon";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { StateDot } from "../../ui/StateDot";
import { fileIcon } from "../files/fileIcons";
import type { CombinedTab } from "../shell/model";

interface TabStripProps {
  tabs: readonly CombinedTab[];
  activeKey?: string;
  canMutate: boolean;
  canSplit: boolean;
  /** Draws a shape as well as a color in each activity dot. */
  stateGlyphs: boolean;
  onSelect(tab: CombinedTab): void;
  onClose(tab: CombinedTab): void;
  onMove(tab: CombinedTab, direction: "left" | "right"): void;
  onRenameTerminal(tab: Extract<CombinedTab, { kind: "terminal" }>): void;
  /** Double-clicking a preview tab makes it permanent, as VS Code's does. */
  onPin(tab: Extract<CombinedTab, { kind: "app" }>): void;
  onNewTerminal(): void;
  onSplit(): void;
}

/**
 * A document tab shows the same icon its Explorer row does — one mapping, two
 * surfaces, so a `.rs` tab and its row cannot disagree about what the file is.
 * A diff is not a file type: it keeps the diff mark and the tint the strip's
 * own rule gives it.
 */
function TabGlyph({ tab }: { tab: Extract<CombinedTab, { kind: "app" }> }) {
  const { icon, color } = tab.appKind === "gitDiff"
    ? { icon: "diff" as const, color: "var(--state-working)" }
    // The strip already shows the basename; `openFileTab` put it there.
    : fileIcon({ name: tab.title, kind: "file" });
  return <span className={`tab-glyph ${tab.appKind}`} style={{ color }}><Icon name={icon} size={12} /></span>;
}

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
  const tabs = useRef<HTMLDivElement>(null);
  // The strip scrolls rather than pushing its neighbours, which means the tab
  // that just became active can be outside it. Measured with the right panel
  // open and four long tmux window names: opening a git diff created its
  // document tab at x=959 in a strip clipped at 923, so the surface took over
  // the window while the tab representing it — and its close button — could
  // not be seen or clicked. `nearest` scrolls the minimum distance and does
  // nothing when the tab is already visible.
  useEffect(() => {
    if (!props.activeKey) return;
    tabs.current?.querySelector<HTMLElement>(`#${CSS.escape(workspaceTabDomId(props.activeKey))}`)
      ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [props.activeKey, props.tabs]);

  const onTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, tab: CombinedTab, index: number) => {
    if (isContextMenuKey(event)) {
      event.preventDefault();
      setMenu({ tab, anchor: anchorForElement(event.currentTarget) });
      return;
    }
    let next = index;
    if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowRight") next = Math.min(props.tabs.length - 1, index + 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = props.tabs.length - 1;
    // Enter and Space are deliberately not handled: a <button> already
    // activates on both, and intercepting them here fired onSelect twice —
    // which for a terminal tab meant two selectWindow actions sharing one
    // captured generation, the second liable to be rejected as stale.
    else return;
    event.preventDefault();
    event.currentTarget.closest("[role=tablist]")?.querySelectorAll<HTMLButtonElement>("[role=tab]")[next]?.focus();
  };

  return <div className="tabstrip">
    <div aria-label="Terminal tabs and documents" className="tabstrip-tabs" ref={tabs} role="tablist">
      {props.tabs.map((tab, index) => {
        const active = tab.key === props.activeKey;
        // `role="presentation"`: a generic element between a tablist and its
        // tabs breaks ownership, and assistive technology then cannot say
        // "tab 3 of 5".
        return <div className={active ? "tab active" : "tab"} key={tab.key} role="presentation">
          <button
            // Only the selected tab's panel exists in the DOM, so only the
            // selected tab may claim to control one.
            aria-controls={active ? workspaceTabPanelDomId(tab.key) : undefined}
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
              // Opening a menu is not a selection: selecting first would make a
              // right-click on a terminal tab issue a real tmux select-window.
              event.preventDefault();
              setMenu({ tab, anchor: { x: event.clientX, y: event.clientY } });
            }}
            onDoubleClick={() => {
              if (tab.kind === "app") props.onPin(tab);
              else if (props.canMutate) props.onRenameTerminal(tab);
            }}
            onKeyDown={(event) => onTabKeyDown(event, tab, index)}
            role="tab"
            // With nothing selected — a window closed by another client, briefly —
            // a roving tabindex of all -1 makes the whole strip unreachable.
            tabIndex={active || (!props.activeKey && index === 0) ? 0 : -1}
            title={tab.kind === "app" ? tab.resource : `tmux window ${tab.id}`}
            type="button"
          >
            {tab.kind === "terminal"
              ? <span className="tab-index">{tab.index}</span>
              : <TabGlyph tab={tab} />}
            <span className={tab.kind === "app" && tab.preview ? "tab-title tab-title-preview" : "tab-title"}>{tab.title}</span>
            {tab.kind === "terminal" && tab.zoomed && <span aria-label="Pane zoomed" className="tab-zoom"><Icon name="zoom" size={11} /></span>}
            {tab.kind === "terminal" && tab.attention !== "none" && <StateDot
              className="tab-dot"
              glyphs={props.stateGlyphs}
              label={`Agent ${tab.attention}`}
              state={tab.attention}
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

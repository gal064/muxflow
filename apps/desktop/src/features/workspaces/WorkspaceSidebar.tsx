import { useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import type { Session } from "../../app/types";
import type { CommandId } from "../../commands/registry";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { needsAttention, nextSortMode, type AgentListRow, type AgentSortMode } from "../agents/agentsList";
import type { AgentAdapterDescriptor, AgentAdapterId, AgentDisplayState, AgentPlacement, AgentRecord } from "../agents/types";
import type { ConnectionPhase } from "../../state/connectionReducer";
import { AGENTS_SECTION_MAX_RATIO, AGENTS_SECTION_MIN_RATIO, SIDEBAR_MIN_WIDTH } from "../shell/types";
import type { WorkspaceRowModel } from "./workspaceRows";

export type WorkspaceCommandId = Extract<CommandId, "session.rename" | "session.moveLeft" | "session.moveRight" | "session.close">;

interface WorkspaceSidebarProps {
  rows: readonly WorkspaceRowModel[];
  agents: readonly AgentListRow[];
  adapters: readonly AgentAdapterDescriptor[];
  agentSort: AgentSortMode;
  /** Draws a shape as well as a color in each state dot. */
  stateGlyphs: boolean;
  /** Share of the sidebar's height given to the agents section. */
  agentsRatio: number;
  canMutate: boolean;
  hostLabel: string;
  transport: "local" | "ssh";
  latencyMs?: number;
  phase: ConnectionPhase;
  onSelectWorkspace(sessionId: string): void;
  onWorkspaceCommand(session: Session, commandId: WorkspaceCommandId): void;
  onSelectAgent(row: AgentListRow): void;
  onSortMode(mode: AgentSortMode): void;
  onAgentsRatio(ratio: number): void;
  /** Current width in CSS pixels, already clamped against the window. */
  width: number;
  /** The cap the caller applies — a third of the window. */
  maxWidth: number;
  onWidth(width: number): void;
  onOpenSettings(): void;
  onLaunchAgent(adapter: AgentAdapterId, placement: AgentPlacement): void;
  onResumeAgent(agent: AgentRecord, placement: AgentPlacement): void;
  onRenameAgent(agent: AgentRecord): void;
  onReviewHooks(adapter: AgentAdapterId, action: "install" | "uninstall"): void;
}

const STATE_GLYPH: Record<AgentDisplayState, string> = {
  blocked: "!",
  done: "✓",
  working: "•",
  unknown: "?",
  idle: "",
};

/**
 * The one 240px rail: workspaces on top, a flat agents list below, the host row
 * at the bottom. It replaces three columns (a 176px icon rail, a 246px
 * Explorer/Git sidebar, a 258px agents panel) that together took 680px of every
 * window regardless of size.
 *
 * The two sections are the Herdr model the user approved: the top answers
 * "where is work happening", the bottom answers "who needs me", and they are
 * separated by a divider the user can drag. The agents list is global on
 * purpose — Herdr shipped a per-workspace scope filter and then deleted it —
 * so the only control in that header is the ordering.
 */
export function WorkspaceSidebar(props: WorkspaceSidebarProps) {
  const [menu, setMenu] = useState<{ session: Session; anchor: ContextMenuAnchor; index: number }>();
  // Launching, resuming, renaming and hook review used to be four permanently
  // visible affordances in the agents panel. They are right-click menus now:
  // the section header for "start something", a row for "do something to this".
  const [agentMenu, setAgentMenu] = useState<{ row?: AgentListRow; anchor: ContextMenuAnchor }>();
  const container = useRef<HTMLElement>(null);

  const focusRelative = (event: KeyboardEvent<HTMLElement>, selector: string, index: number, length: number) => {
    const delta = event.key === "ArrowUp" ? -1 : event.key === "ArrowDown" ? 1 : 0;
    if (!delta) return;
    event.preventDefault();
    const next = Math.max(0, Math.min(length - 1, index + delta));
    container.current?.querySelectorAll<HTMLElement>(selector)[next]?.focus();
  };

  const startSectionDrag = (event: PointerEvent<HTMLElement>) => {
    const bounds = container.current?.getBoundingClientRect();
    if (!bounds || bounds.height <= 0) return;
    const target = event.currentTarget;
    target.setPointerCapture(event.pointerId);
    const move = (pointer: globalThis.PointerEvent) => {
      props.onAgentsRatio((bounds.bottom - pointer.clientY) / bounds.height);
    };
    const stop = () => {
      target.removeEventListener("pointermove", move as EventListener);
      target.removeEventListener("pointerup", stop);
      target.removeEventListener("pointercancel", stop);
    };
    target.addEventListener("pointermove", move as EventListener);
    target.addEventListener("pointerup", stop);
    target.addEventListener("pointercancel", stop);
  };

  return <nav aria-label="Workspaces and agents" className="sidebar" ref={container}>
    <div className="sidebar-section sidebar-workspaces">
      <div className="section-label" id="sidebar-workspaces-label">Workspaces</div>
      <div aria-labelledby="sidebar-workspaces-label" className="sidebar-scroll" role="list">
        {props.rows.length === 0
          ? <p className="quiet-empty">No tmux sessions on this host yet.</p>
          : props.rows.map((row, index) => <div className="workspace-row" key={row.session.id} role="listitem">
            <button
              aria-current={row.active ? "true" : undefined}
              // The badge beside this row is a decorative span, so the count
              // has to be part of the row's own name to be announced at all.
              aria-label={[
                row.session.name,
                row.activity,
                row.unread > 0 ? `${row.unread} agent${row.unread === 1 ? "" : "s"} waiting` : undefined,
                row.metadata,
              ].filter(Boolean).join(", ")}
              className={row.active ? "workspace-button active" : "workspace-button"}
              data-workspace-index={index}
              onClick={() => props.onSelectWorkspace(row.session.id)}
              onContextMenu={(event) => {
                event.preventDefault();
                setMenu({ session: row.session, anchor: { x: event.clientX, y: event.clientY }, index });
              }}
              onDoubleClick={() => props.canMutate && props.onWorkspaceCommand(row.session, "session.rename")}
              onKeyDown={(event) => {
                if (isContextMenuKey(event)) {
                  event.preventDefault();
                  setMenu({ session: row.session, anchor: anchorForElement(event.currentTarget), index });
                  return;
                }
                focusRelative(event, "[data-workspace-index]", index, props.rows.length);
              }}
              type="button"
            >
              <span className="workspace-title">
                {row.working && <span aria-hidden="true" className="spinner" />}
                <span className="workspace-name">{row.session.name}</span>
              </span>
              {row.activity && <span className="workspace-activity">{row.activity}</span>}
              {row.metadata && <span className="workspace-meta">{row.metadata}</span>}
            </button>
            {row.unread > 0 && <span aria-hidden="true" className="badge badge-row">{row.unread > 99 ? "99+" : row.unread}</span>}
          </div>)}
      </div>
    </div>

    <div
      aria-label="Resize the agents section"
      aria-orientation="horizontal"
      aria-valuemax={Math.round(AGENTS_SECTION_MAX_RATIO * 100)}
      aria-valuemin={Math.round(AGENTS_SECTION_MIN_RATIO * 100)}
      aria-valuenow={Math.round(props.agentsRatio * 100)}
      className="section-divider"
      onKeyDown={(event) => {
        const delta = event.key === "ArrowUp" ? 0.04 : event.key === "ArrowDown" ? -0.04 : 0;
        if (!delta) return;
        event.preventDefault();
        props.onAgentsRatio(props.agentsRatio + delta);
      }}
      onPointerDown={startSectionDrag}
      role="separator"
      tabIndex={0}
    />

    <div
      className="sidebar-section sidebar-agents"
      onContextMenu={(event) => {
        if (!(event.target as HTMLElement).closest(".agent-button")) {
          event.preventDefault();
          setAgentMenu({ anchor: { x: event.clientX, y: event.clientY } });
        }
      }}
      style={{ flexBasis: `${Math.round(props.agentsRatio * 100)}%` }}
    >
      <div className="section-head">
        <span className="section-label" id="sidebar-agents-label">Agents</span>
        <button
          aria-label={`Agent ordering: ${props.agentSort}. Switch to ${nextSortMode(props.agentSort)}.`}
          className="sort-toggle"
          onClick={() => props.onSortMode(nextSortMode(props.agentSort))}
          // The section's launch and hook actions are on its context menu; this
          // is the focusable thing in the header, so it is the keyboard's way in.
          onKeyDown={(event) => {
            if (!isContextMenuKey(event)) return;
            event.preventDefault();
            setAgentMenu({ anchor: anchorForElement(event.currentTarget) });
          }}
          type="button"
        >{props.agentSort}</button>
      </div>
      <div aria-labelledby="sidebar-agents-label" className="sidebar-scroll" role="list">
        {props.agents.length === 0
          ? <p className="quiet-empty">No agents detected.</p>
          : props.agents.map((row, index) => <div className="agent-row" key={row.agent.id} role="listitem">
            <button
              // Same reason as the workspace row: the badge is decorative, and
              // "waiting" is the whole point of this list.
              aria-label={[
                row.agent.displayName,
                row.state,
                needsAttention(row.state) ? "waiting" : undefined,
                row.location.workspaceName,
                row.location.tabIndex === undefined ? undefined : `tab ${row.location.tabIndex}`,
                row.routable ? undefined : "unmapped, navigation unavailable",
              ].filter(Boolean).join(", ")}
              className="agent-button"
              data-agent-index={index}
              disabled={!row.routable}
              onClick={() => props.onSelectAgent(row)}
              onContextMenu={(event) => {
                event.preventDefault();
                setAgentMenu({ row, anchor: { x: event.clientX, y: event.clientY } });
              }}
              onKeyDown={(event) => {
                if (isContextMenuKey(event)) {
                  event.preventDefault();
                  setAgentMenu({ row, anchor: anchorForElement(event.currentTarget) });
                  return;
                }
                focusRelative(event, "[data-agent-index]", index, props.agents.length);
              }}
              title={row.routable
                ? `${row.agent.displayName} · ${row.state} · ${row.location.workspaceName}`
                : `${row.agent.displayName} has no exact pane; navigation is unavailable`}
              type="button"
            >
              <span className="agent-line">
                {/* Decorative: the row button's own accessible name already
                    says the state, and a role="img" here announced it twice. */}
                <span aria-hidden="true" className={`state-dot `}>{props.stateGlyphs ? STATE_GLYPH[row.state] : ""}</span>
                <span className="agent-location">{row.location.workspaceName}</span>
                {row.location.tabIndex !== undefined && <span className="agent-tab">{row.location.tabIndex}</span>}
              </span>
              <span className="agent-detail">
                {row.agent.displayName} · {row.state}{row.routable ? "" : " · unmapped"}
              </span>
            </button>
            {needsAttention(row.state) && <span aria-hidden="true" className="badge badge-row">1</span>}
          </div>)}
      </div>
    </div>

    <button
      aria-label={`Host ${props.hostLabel} over ${props.transport}, ${props.phase}. Open connection settings.`}
      className="host-row"
      onClick={props.onOpenSettings}
      title={props.latencyMs === undefined
        ? `${props.hostLabel} · ${props.transport} · ${props.phase}`
        : `${props.hostLabel} · ${props.transport} · ${props.phase} · last measured round-trip ${Math.round(props.latencyMs)} ms`}
      type="button"
    >
      <span aria-hidden="true" className={`link-dot ${props.phase}`} />
      <span className="host-name">{props.hostLabel}</span>
      <span className="host-transport">{props.transport}</span>
      {props.latencyMs !== undefined && <span className="host-latency">{Math.round(props.latencyMs)} ms</span>}
    </button>

    {menu && <ContextMenu
      anchor={menu.anchor}
      items={[
        { id: "rename", label: "Rename workspace…", disabled: !props.canMutate, run: () => props.onWorkspaceCommand(menu.session, "session.rename") },
        { id: "up", label: "Move up", disabled: !props.canMutate || menu.index === 0, run: () => props.onWorkspaceCommand(menu.session, "session.moveLeft") },
        { id: "down", label: "Move down", disabled: !props.canMutate || menu.index === props.rows.length - 1, run: () => props.onWorkspaceCommand(menu.session, "session.moveRight") },
        "separator",
        { id: "close", label: "Close workspace…", destructive: true, disabled: !props.canMutate, run: () => props.onWorkspaceCommand(menu.session, "session.close") },
      ]}
      label={`Actions for ${menu.session.name}`}
      onClose={() => setMenu(undefined)}
    />}

    {/* The sidebar's own width. The token table calls for 240px minimum,
        drag-resizable and capped at a third of the window; the cap is applied
        by the caller, which is the only thing that knows the window. */}
    <div
      aria-label="Resize the sidebar"
      aria-orientation="vertical"
      aria-valuemax={Math.round(props.maxWidth)}
      aria-valuemin={SIDEBAR_MIN_WIDTH}
      aria-valuenow={Math.round(props.width)}
      className="sidebar-resize"
      onKeyDown={(event) => {
        const delta = event.key === "ArrowLeft" ? -16 : event.key === "ArrowRight" ? 16 : 0;
        if (!delta) return;
        event.preventDefault();
        props.onWidth(props.width + delta);
      }}
      onPointerDown={(event) => {
        const left = container.current?.getBoundingClientRect().left ?? 0;
        const target = event.currentTarget;
        target.setPointerCapture(event.pointerId);
        const move = (pointer: globalThis.PointerEvent) => props.onWidth(pointer.clientX - left);
        const stop = () => {
          target.removeEventListener("pointermove", move as EventListener);
          target.removeEventListener("pointerup", stop);
        };
        target.addEventListener("pointermove", move as EventListener);
        target.addEventListener("pointerup", stop);
      }}
      role="separator"
      tabIndex={0}
    />

    {agentMenu && <ContextMenu
      anchor={agentMenu.anchor}
      items={agentMenu.row
        ? [
          { id: "focus", label: "Focus this agent's pane", disabled: !agentMenu.row.routable, run: () => props.onSelectAgent(agentMenu.row!) },
          { id: "rename", label: "Rename agent…", disabled: !props.canMutate, run: () => props.onRenameAgent(agentMenu.row!.agent) },
          ...resumePlacements(props.adapters, agentMenu.row.agent).map((placement) => ({
            id: `resume-${placement}`,
            label: `Resume in new ${placement}`,
            disabled: !props.canMutate,
            run: () => props.onResumeAgent(agentMenu.row!.agent, placement),
          })),
        ]
        : launchItems(props.adapters, props.canMutate, props.onLaunchAgent, props.onReviewHooks)}
      label={agentMenu.row ? `Actions for ${agentMenu.row.agent.displayName}` : "Agent actions"}
      onClose={() => setAgentMenu(undefined)}
    />}
  </nav>;
}

function resumePlacements(adapters: readonly AgentAdapterDescriptor[], agent: AgentRecord): AgentPlacement[] {
  const adapter = adapters.find((item) => item.id === agent.adapterId);
  return adapter?.supportsResume ? adapter.placements : [];
}

function launchItems(
  adapters: readonly AgentAdapterDescriptor[],
  canMutate: boolean,
  onLaunch: (adapter: AgentAdapterId, placement: AgentPlacement) => void,
  onReviewHooks: (adapter: AgentAdapterId, action: "install" | "uninstall") => void,
) {
  const launches = adapters
    .filter((adapter) => adapter.supportsLaunch)
    .flatMap((adapter) => adapter.placements.map((placement) => ({
      id: `launch-${adapter.id}-${placement}`,
      label: `New ${adapter.displayName} in a ${placement}`,
      disabled: !canMutate,
      run: () => onLaunch(adapter.id, placement),
    })));
  const hooks = adapters
    .filter((adapter) => adapter.supportsHooks)
    .flatMap((adapter) => (["install", "uninstall"] as const).map((action) => ({
      id: `hooks-${adapter.id}-${action}`,
      label: `${action === "install" ? "Review" : "Remove"} ${adapter.displayName} hooks…`,
      disabled: !canMutate,
      run: () => onReviewHooks(adapter.id, action),
    })));
  if (launches.length === 0 && hooks.length === 0) {
    return [{ id: "none", label: "No agent adapters available", disabled: true, run: () => undefined }];
  }
  return hooks.length > 0 ? [...launches, "separator" as const, ...hooks] : launches;
}

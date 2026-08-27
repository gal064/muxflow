import { useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent } from "react";
import type { Session } from "../../app/types";
import type { CommandId } from "../../commands/registry";
import { usePublishedRowCommands, type RowCommandSource } from "../../commands/rowCommands";
import { anchorForElement, ContextMenu, isContextMenuKey, type ContextMenuAnchor } from "../../ui/ContextMenu";
import { AgentStateIndicator } from "../../ui/AgentStateIndicator";
import { Icon } from "../../ui/Icon";
import {
  groupAgentRows, groupAgentRowsByStatus, needsAttention, nextSortMode, sortModeLabel,
  type AgentListRow, type AgentSortMode, type AgentWorkspaceGroup,
} from "../agents/agentsList";
import { AgentMark } from "../agents/AgentIdentity";
import { agentSessionLabel } from "../agents/agentLabels";
import type { AgentAdapterDescriptor, AgentAdapterId, AgentDisplayState, AgentPlacement, AgentRecord } from "../agents/types";
import type { ConnectionPhase } from "../../state/connectionReducer";
import { AGENTS_SECTION_MAX_RATIO, AGENTS_SECTION_MIN_RATIO, SIDEBAR_MIN_WIDTH } from "../shell/types";
import { sameHostConnection, type HostScopeToken } from "../shell/hostScope";
import { activityWord, type WorkspaceRowModel } from "./workspaceRows";
import { useTransientDrag } from "./transientDrag";

export type WorkspaceCommandId = Extract<CommandId, "session.rename" | "session.moveLeft" | "session.moveRight" | "session.close">;

interface WorkspaceSidebarProps {
  rows: readonly WorkspaceRowModel[];
  agents: readonly AgentListRow[];
  adapters: readonly AgentAdapterDescriptor[];
  agentSort: AgentSortMode;
  /** Omits the per-agent detail lines inside each workspace summary. */
  compactWorkspaces: boolean;
  /** Draws a shape as well as a color in each state dot. */
  stateGlyphs: boolean;
  /** Share of the sidebar's height given to the agents section. */
  agentsRatio: number;
  canMutate: boolean;
  commandScope: HostScopeToken;
  hostLabel: string;
  transport: "local" | "ssh";
  latencyMs?: number;
  phase: ConnectionPhase;
  /** The list's one filter: pinned workspaces only, plus whatever is selected. */
  pinnedOnly: boolean;
  onTogglePinnedOnly(): void;
  onSelectWorkspace(sessionId: string): void;
  /**
   * Shift-click, and the menu item beside it: pins the workspace to the top of
   * this list, or unpins one already there.
   */
  onTogglePinnedWorkspace(session: Session, scope: HostScopeToken): void;
  onWorkspaceCommand(session: Session, commandId: WorkspaceCommandId, scope: HostScopeToken): void;
  onSelectAgent(row: AgentListRow, scope: HostScopeToken): void;
  onSortMode(mode: AgentSortMode): void;
  onAgentsRatio(ratio: number): void;
  /** Current width in CSS pixels, already clamped against the window. */
  width: number;
  /** The cap the caller applies — a third of the window. */
  maxWidth: number;
  onWidth(width: number): void;
  onOpenSettings(): void;
  onLaunchAgent(adapter: AgentAdapterId, placement: AgentPlacement): void;
  onResumeAgent(agent: AgentRecord, placement: AgentPlacement, scope: HostScopeToken): void;
  onRenameAgent(agent: AgentRecord, scope: HostScopeToken): void;
  onReviewHooks(adapter: AgentAdapterId, action: "install" | "uninstall"): void;
  /**
   * Set when this host cannot report agent status at all. The section says so
   * once, rather than letting rows imply a state nothing feeds.
   */
  hookNotice?: string;
  /** Present when the notice is something the user can act on. */
  onSetUpHost?(): void;
}

/**
 * The one 240px rail: workspaces on top, an agents list below, the host row
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
  const [menu, setMenu] = useState<{ session: Session; anchor: ContextMenuAnchor; index: number; scope: HostScopeToken }>();
  // Launching, resuming, renaming and hook review used to be four permanently
  // visible affordances in the agents panel. They are right-click menus now:
  // the section header for "start something", a row for "do something to this".
  const [agentMenu, setAgentMenu] = useState<{ row?: AgentListRow; anchor: ContextMenuAnchor; scope: HostScopeToken }>();
  const [focusedAgentTarget, setFocusedAgentTarget] = useState<{ id: string; scope: HostScopeToken }>();
  const container = useRef<HTMLElement>(null);
  const [displayedAgentsRatio, startAgentsRatioDrag] = useTransientDrag(props.agentsRatio, props.onAgentsRatio);
  const [displayedWidth, startWidthDrag] = useTransientDrag(props.width, props.onWidth);
  /**
   * Where the right-clicked workspace sits in *tmux's* order, not on screen.
   *
   * `session.moveLeft` and `session.moveRight` are tmux reorders: they step the
   * session's `order`, so their bounds are that order and not the display order
   * the pinned block rearranged. Gating on the display index made a pinned row
   * refuse to move up while it still had somewhere to go, and offered "move up"
   * to the last row on screen, where it computed a negative index and silently
   * did nothing.
   */
  const moveIndex = menu
    ? [...props.rows]
      .sort((left, right) => (left.session.order ?? 0) - (right.session.order ?? 0))
      .findIndex((row) => row.session.id === menu.session.id)
    : -1;
  const groupedAgents = props.agentSort === "workspace" ? groupAgentRows(props.agents) : [];
  const priorityAgents = props.agentSort === "workspace" ? [] : groupAgentRowsByStatus(props.agents);
  // The flat position in `props.agents`, kept across every grouping: the roving
  // arrow keys walk `[data-agent-index]` in document order, and a per-group
  // index would restart the walk at every heading.
  const agentIndexes = new Map(props.agents.map((row, index) => [row, index]));

  // Resolved against the live list every render: an agent whose pane closed
  // drops out of `props.agents`, and the palette must stop offering to focus it
  // at the same moment its row stops being clickable.
  const focusedAgent = focusedAgentTarget && sameHostConnection(focusedAgentTarget.scope, props.commandScope)
    ? props.agents.find((row) => row.agent.id === focusedAgentTarget.id)
    : undefined;
  const focusedAgentResume = focusedAgent ? resumePlacements(props.adapters, focusedAgent.agent)[0] : undefined;
  const rowActions = useMemo<readonly CommandId[]>(() => {
    if (!focusedAgent) return [];
    const ids: CommandId[] = [];
    if (focusedAgent.routable) ids.push("agents.focusRow");
    if (props.canMutate) ids.push("agents.renameRow");
    if (props.canMutate && focusedAgentResume) ids.push("agents.resumeRow");
    return ids;
  }, [focusedAgent, focusedAgentResume, props.canMutate]);
  const runRowCommand = useRef<(commandId: CommandId) => void>(() => undefined);
  runRowCommand.current = (commandId) => {
    if (!focusedAgent) return;
    switch (commandId) {
      case "agents.focusRow": props.onSelectAgent(focusedAgent, focusedAgentTarget!.scope); return;
      case "agents.renameRow": props.onRenameAgent(focusedAgent.agent, focusedAgentTarget!.scope); return;
      // The context menu offers one item per placement because it has room to.
      // The palette is a single line, so it takes the adapter's first declared
      // placement rather than inventing a second prompt to ask which.
      case "agents.resumeRow": if (focusedAgentResume) props.onResumeAgent(focusedAgent.agent, focusedAgentResume, focusedAgentTarget!.scope); return;
    }
  };
  const rowSource = useMemo<RowCommandSource | undefined>(() => rowActions.length === 0 || !focusedAgent ? undefined : {
    subject: focusedAgent.agent.displayName,
    available: rowActions,
    run: (commandId) => runRowCommand.current(commandId),
  }, [focusedAgent, rowActions]);
  usePublishedRowCommands("agents", rowSource);

  /**
   * The roving arrow keys, walked in document order.
   *
   * The number in the attribute is the row's flat position in `props.agents`,
   * and in the priority mode that is deliberately *not* the reading order:
   * `compareAgents` ranks done-unread above working, while the headings read
   * Blocked → Working → Done → Idle. Stepping the number and then taking the
   * query result at that position walked two orders at once, so a list holding
   * both a done and a working agent skipped a row going down and stuck at the
   * seam. The DOM is the only thing that knows the reading order, so the step
   * happens in it: find where this row sits among the marked rows, move one,
   * focus what is there. The attribute is then only an identity.
   */
  const focusRelative = (event: KeyboardEvent<HTMLElement>, attribute: string, index: number) => {
    const delta = event.key === "ArrowUp" ? -1 : event.key === "ArrowDown" ? 1 : 0;
    if (!delta) return;
    event.preventDefault();
    const ordered = [...container.current?.querySelectorAll<HTMLElement>(`[${attribute}]`) ?? []];
    const here = ordered.findIndex((element) => element.getAttribute(attribute) === String(index));
    if (here < 0) return;
    ordered[Math.max(0, Math.min(ordered.length - 1, here + delta))]?.focus();
  };

  const startSectionDrag = (event: PointerEvent<HTMLElement>) => {
    const bounds = container.current?.getBoundingClientRect();
    if (!bounds || bounds.height <= 0) return;
    startAgentsRatioDrag(event, (pointer) => Math.max(
      AGENTS_SECTION_MIN_RATIO,
      Math.min(AGENTS_SECTION_MAX_RATIO, (bounds.bottom - pointer.clientY) / bounds.height),
    ));
  };

  const renderAgentRow = (row: AgentListRow, index: number, key = row.agent.id) => {
    const sessionLabel = agentSessionLabel(row.agent, props.adapters);
    const detail = [
      props.agentSort === "workspace" ? undefined : row.location.workspaceName,
      row.location.tabIndex === undefined ? undefined : `tab ${row.location.tabIndex}`,
      row.state,
      row.routable ? undefined : "unmapped",
    ].filter(Boolean).join(" · ");
    return <div className="agent-row" key={key} role="listitem">
      <button
        aria-label={[
          sessionLabel,
          row.state,
          needsAttention(row.state) ? "waiting" : undefined,
          row.location.workspaceName,
          row.location.hostLabel,
          row.location.tabIndex === undefined ? undefined : `tab ${row.location.tabIndex}`,
          row.routable ? undefined : "unmapped, navigation unavailable",
        ].filter(Boolean).join(", ")}
        className="agent-button"
        data-agent-index={index}
        aria-disabled={!row.routable}
        data-unavailable={row.routable ? undefined : "true"}
        onClick={() => { if (row.routable) props.onSelectAgent(row, props.commandScope); }}
        onContextMenu={(event) => {
          event.preventDefault();
          setFocusedAgentTarget({ id: row.agent.id, scope: props.commandScope });
          setAgentMenu({ row, anchor: { x: event.clientX, y: event.clientY }, scope: props.commandScope });
        }}
        onFocus={() => setFocusedAgentTarget({ id: row.agent.id, scope: props.commandScope })}
        onPointerDown={() => setFocusedAgentTarget({ id: row.agent.id, scope: props.commandScope })}
        onKeyDown={(event) => {
          if (isContextMenuKey(event)) {
            event.preventDefault();
            setAgentMenu({ row, anchor: anchorForElement(event.currentTarget), scope: props.commandScope });
            return;
          }
          focusRelative(event, "data-agent-index", index);
        }}
        title={`${sessionLabel} · ${row.agent.displayName} · ${row.state} · ${row.location.workspaceName} · ${row.location.hostLabel || props.hostLabel}${row.routable ? "" : " · navigation unavailable"}`}
        type="button"
      >
        <span className="agent-line">
          {/* Icon and state are one object here — see `AgentMark`. */}
          <AgentMark adapterId={row.agent.adapterId} glyphs={props.stateGlyphs} state={row.state} />
          {/* Name and pin are one cell so the pin hugs the end of the text
              rather than floating out at the row's edge, and so the detail
              column keeps the width it had before any pin existed. */}
          <span className="agent-name-cell">
            <span className="agent-session-label">{sessionLabel}</span>
            {row.location.tabPinnedAt !== undefined
              && <span aria-hidden="true" className="agent-pin"><Icon name="pin" size={11} /></span>}
          </span>
          <span className="agent-detail">{detail}</span>
        </span>
      </button>
      {needsAttention(row.state) && <span aria-hidden="true" className="badge badge-row">1</span>}
    </div>;
  };

  const renderWorkspaceRow = (row: WorkspaceRowModel, index: number) => <div className="workspace-row" key={row.session.id} role="listitem">
    <button
      aria-current={row.active ? "true" : undefined}
      // The badge beside this row is a decorative span, so the count
      // has to be part of the row's own name to be announced at all.
      // The row lists up to three agents; the label names the loudest
      // and counts the rest. Reading every line back would make a busy
      // workspace four announcements long for one list item.
      aria-label={rowLabel(row)}
      className={["workspace-button", row.active ? "active" : undefined, props.compactWorkspaces ? "compact" : undefined].filter(Boolean).join(" ")}
      data-workspace-index={index}
      // Shift-click pins rather than selects, as it does in the tab
      // strip. Pinning a workspace must not navigate to it: the row
      // being moved to the top is often not the one you are working in.
      onClick={(event) => event.shiftKey
        ? props.onTogglePinnedWorkspace(row.session, props.commandScope)
        : props.onSelectWorkspace(row.session.id)}
      onContextMenu={(event) => {
        event.preventDefault();
        setMenu({ session: row.session, anchor: { x: event.clientX, y: event.clientY }, index, scope: props.commandScope });
      }}
      onDoubleClick={() => props.canMutate && props.onWorkspaceCommand(row.session, "session.rename", props.commandScope)}
      onKeyDown={(event) => {
        if (isContextMenuKey(event)) {
          event.preventDefault();
          setMenu({ session: row.session, anchor: anchorForElement(event.currentTarget), index, scope: props.commandScope });
          return;
        }
        focusRelative(event, "data-workspace-index", index);
      }}
      type="button"
    >
      {/* Inside the title, not above it: the number is the row's
          address, and a line of its own put a column of digits beside
          the names rather than in front of them. */}
      <span className="workspace-title">
        {index < 9 && <span aria-hidden="true" className="workspace-shortcut-index">{index + 1}</span>}
        {/* One workspace-level indicator, in the same place in both
            modes. Non-compact shows it for Working alone, because its
            agent lines below already report blocked and done in words;
            compact has only the right-edge cluster, whose marks are
            per-agent and read at 6px, so the row itself said nothing
            about the workspace at all. Here it is the loudest state
            across the workspace — see `workspaceIndicatorState`. Every
            row keeps the slot whether or not it has anything to say, so
            a state arriving cannot shift the name beside it. */}
        {props.compactWorkspaces
          ? <WorkspaceStateIndicator glyphs={props.stateGlyphs} row={row} />
          : row.working ? <span aria-hidden="true" className="spinner" /> : <HiddenStateSlot />}
        <span className="workspace-name">{row.session.name}</span>
      </span>
      {/* Compact rows trade the per-agent lines for a cluster of
          badged marks on the same line: which adapters are here and
          how each is doing, in the width three words would have taken.
          Decorative, like the lines it replaces — `rowLabel` is the
          row's whole accessible name either way. */}
      {props.compactWorkspaces && row.agents.length > 0 && <span aria-hidden="true" className="workspace-agents">
        {row.agents.map((agent) => <AgentMark
          adapterId={agent.adapterId}
          glyphs={props.stateGlyphs}
          key={agent.id}
          state={agent.state}
        />)}
        {row.agentOverflow > 0 && <span className="workspace-agents-more">+{row.agentOverflow}</span>}
      </span>}
      {props.compactWorkspaces && row.unread > 0
        && <span aria-hidden="true" className="badge badge-inline">{row.unread > 99 ? "99+" : row.unread}</span>}
      {!props.compactWorkspaces && row.agents.length > 0 && <span className="workspace-activity">
        {row.agents.map((agent) => <span className="workspace-activity-line" key={agent.id}>
          {/* Decorative: the button's own accessible name already
              carries the loudest agent and the total. */}
          <AgentMark adapterId={agent.adapterId} glyphs={props.stateGlyphs} state={agent.state} />
          <span className="workspace-activity-text">{agentLine(agent)}</span>
        </span>)}
        {row.agentOverflow > 0 && <span className="workspace-activity-line workspace-activity-more">
          <span className="workspace-activity-text">…{row.agentOverflow} more</span>
        </span>}
      </span>}
      {/* Neither the branch nor the working directory renders here any
          more: a workspace holds many tabs in many directories, so one
          branch per row was a lie half the time. Both live on in ⌘P's
          match key, where they are something you search rather than
          something you read once per row. */}
    </button>
    {!props.compactWorkspaces && row.unread > 0
      && <span aria-hidden="true" className="badge badge-row">{row.unread > 99 ? "99+" : row.unread}</span>}
  </div>;

  /**
   * The two blocks the dividers label, or nothing when no workspace is pinned.
   *
   * Each row keeps the flat position it has in `props.rows`, because that
   * number is the row's address: it is what the digit beside the name says and
   * what ⌘1–9 selects, and restarting it per block would make the second block
   * claim shortcuts the first already owns.
   */
  const numberedRows = props.rows.map((row, index) => ({ row, index }));
  const workspaceBlocks = numberedRows.some((entry) => entry.row.pinned)
    ? [
      { key: "pinned", label: "Pinned", entries: numberedRows.filter((entry) => entry.row.pinned) },
      { key: "others", label: "Others", entries: numberedRows.filter((entry) => !entry.row.pinned) },
    ].filter((block) => block.entries.length > 0)
    : undefined;

  /**
   * One workspace's agents under their own heading. The host is in the tooltip
   * and not on the line: one connection is live at a time on this branch, so
   * printing it on every heading spent the row's whole right edge repeating a
   * constant, and the count that replaces it is the thing that differs.
   */
  const renderAgentGroup = (group: AgentWorkspaceGroup) => {
    const headingId = `agent-workspace-${encodeURIComponent(group.key)}`;
    return <section aria-labelledby={headingId} className="agent-workspace-group" key={group.key} role="group">
      <h3 className="agent-workspace-heading" id={headingId} title={`${group.workspaceName} · ${group.hostLabel}`}>
        <span>{group.workspaceName}</span>
        <span aria-hidden="true" className="agent-group-count">{group.rows.length}</span>
      </h3>
      {group.rows.map((row) => renderAgentRow(row, agentIndexes.get(row)!, `${group.key}\0${row.agent.id}`))}
    </section>;
  };

  /** The workspace-sort groups under the sidebar's two dividers, or neither. */
  const agentBlocks = groupedAgents.some((group) => group.pinned)
    ? [
      { key: "pinned", label: "Pinned", groups: groupedAgents.filter((group) => group.pinned) },
      { key: "others", label: "Others", groups: groupedAgents.filter((group) => !group.pinned) },
    ].filter((block) => block.groups.length > 0)
    : undefined;

  return <nav
    aria-label="Workspaces and agents"
    className="sidebar"
    ref={container}
    style={{ "--sidebar-width": `${displayedWidth}px` } as CSSProperties}
  >
    <div className="sidebar-section sidebar-workspaces">
      <div className="section-head">
        <span className="section-label" id="sidebar-workspaces-label">Workspaces</span>
        {/* The list's one control, in the place and the shape the agents
            header's ordering toggle already established: it names what the
            next click will show, not what is showing now. */}
        <button
          aria-label={props.pinnedOnly
            ? "Showing pinned workspaces only. Show all workspaces."
            : "Showing all workspaces. Show pinned workspaces only."}
          className="sort-toggle"
          onClick={props.onTogglePinnedOnly}
          type="button"
        >{props.pinnedOnly ? "all" : "pinned"}</button>
      </div>
      <div aria-labelledby="sidebar-workspaces-label" className="sidebar-scroll" role="list">
        {props.rows.length === 0
          ? <p className="quiet-empty">{props.pinnedOnly
            ? "No pinned workspaces — Shift-click a workspace to pin it."
            : "No tmux sessions on this host yet."}</p>
          : workspaceBlocks
            // A pin used to draw a glyph on the row. It draws a divider now:
            // one label for the whole block says what a mark repeated down
            // every pinned row said, and it says it without moving any name.
            ? workspaceBlocks.map((block) => <section
              aria-labelledby={`sidebar-workspaces-${block.key}`}
              className="list-block"
              key={block.key}
              role="group"
            >
              <h3 className="list-divider" id={`sidebar-workspaces-${block.key}`}>{block.label}</h3>
              {block.entries.map((entry) => renderWorkspaceRow(entry.row, entry.index))}
            </section>)
            : props.rows.map((row, index) => renderWorkspaceRow(row, index))}
      </div>
    </div>

    <div
      aria-label="Resize the agents section"
      aria-orientation="horizontal"
      aria-valuemax={Math.round(AGENTS_SECTION_MAX_RATIO * 100)}
      aria-valuemin={Math.round(AGENTS_SECTION_MIN_RATIO * 100)}
      aria-valuenow={Math.round(displayedAgentsRatio * 100)}
      className="section-divider"
      onKeyDown={(event) => {
        const delta = event.key === "ArrowUp" ? 0.04 : event.key === "ArrowDown" ? -0.04 : 0;
        if (!delta) return;
        event.preventDefault();
        props.onAgentsRatio(displayedAgentsRatio + delta);
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
          setAgentMenu({ anchor: { x: event.clientX, y: event.clientY }, scope: props.commandScope });
        }
      }}
      style={{ flexBasis: `${Math.round(displayedAgentsRatio * 100)}%` }}
    >
      <div className="section-head">
        <span className="section-label" id="sidebar-agents-label">Agents</span>
        <button
          aria-label={`Agent ordering: ${sortModeLabel(props.agentSort)}. Switch to ${sortModeLabel(nextSortMode(props.agentSort))}.`}
          className="sort-toggle"
          onClick={() => props.onSortMode(nextSortMode(props.agentSort))}
          // The section's launch and hook actions are on its context menu; this
          // is the focusable thing in the header, so it is the keyboard's way in.
          onKeyDown={(event) => {
            if (!isContextMenuKey(event)) return;
            event.preventDefault();
            setAgentMenu({ anchor: anchorForElement(event.currentTarget), scope: props.commandScope });
          }}
          type="button"
        >{sortModeLabel(props.agentSort)}</button>
      </div>
      {/* Above the list, not instead of it. The rows are real — a detected
          agent exists — and what is missing is any way to know what they are
          doing, which is what this line says and the neutral "unknown" dot on
          each row repeats. Replacing the rows would hide something true to
          avoid saying something honest.

          Text, never a button. The shell's resting-control budget is eight and
          is spent (Phase 11 gate 5); a control that appears on every un-wired
          host — which is this host until it is set up, and permanently after a
          "not now" — is a ninth at rest. The action lives where this section's
          other host actions already live: its context menu, Settings, and the
          one-time prompt itself. */}
      {props.hookNotice && <p className="agents-notice" role="note">{props.hookNotice}</p>}
      <div aria-labelledby="sidebar-agents-label" className="sidebar-scroll" role="list">
        {props.agents.length === 0
          ? <p className="quiet-empty">No agents detected.</p>
          : props.agentSort === "workspace"
            ? agentBlocks
              // The sidebar's dividers, over the same per-workspace groups:
              // a pinned workspace's agents read as one block above the rest
              // rather than as a mark repeated on every group heading.
              ? agentBlocks.map((block) => <section
                aria-labelledby={`agent-block-${block.key}`}
                className="list-block"
                key={block.key}
                role="group"
              >
                <h3 className="list-divider" id={`agent-block-${block.key}`}>{block.label}</h3>
                {block.groups.map(renderAgentGroup)}
              </section>)
              : groupedAgents.map(renderAgentGroup)
            // Priority: the same clustering, keyed on what the agent is doing
            // rather than where it lives. The rows already carry the workspace
            // name in their detail line whenever the sort is not by workspace.
            : priorityAgents.map((group) => {
              const headingId = `agent-status-${group.key}`;
              return <section aria-labelledby={headingId} className="agent-workspace-group" key={group.key} role="group">
                <h3 className="agent-workspace-heading" id={headingId}>
                  {/* The pinned block is not a state, so it draws the same pin
                      its rows' tabs and workspaces do rather than a dot that
                      would claim something about what those agents are doing. */}
                  {group.state
                    ? <AgentStateIndicator
                      className="state-dot agent-group-dot"
                      glyphs={props.stateGlyphs}
                      spinnerClassName="agent-group-dot"
                      state={group.state}
                    />
                    : <span aria-hidden="true" className="agent-group-pin"><Icon name="pin" size={11} /></span>}
                  <span>{group.label}</span>
                  <span aria-hidden="true" className="agent-group-count">{group.rows.length}</span>
                </h3>
                {group.rows.map((row) => renderAgentRow(row, agentIndexes.get(row)!, `${group.key}\0${row.agent.id}`))}
              </section>;
            })}
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
        { id: "rename", label: "Rename workspace…", disabled: !props.canMutate, run: () => props.onWorkspaceCommand(menu.session, "session.rename", menu.scope) },
        // Not gated on `canMutate`: nothing is sent to tmux.
        {
          id: "pin",
          label: props.rows.find((row) => row.session.id === menu.session.id)?.pinned ? "Unpin workspace" : "Pin workspace",
          run: () => props.onTogglePinnedWorkspace(menu.session, menu.scope),
        },
        { id: "up", label: "Move up", disabled: !props.canMutate || moveIndex <= 0, run: () => props.onWorkspaceCommand(menu.session, "session.moveLeft", menu.scope) },
        { id: "down", label: "Move down", disabled: !props.canMutate || moveIndex < 0 || moveIndex === props.rows.length - 1, run: () => props.onWorkspaceCommand(menu.session, "session.moveRight", menu.scope) },
        "separator",
        { id: "close", label: "Close workspace…", destructive: true, disabled: !props.canMutate, run: () => props.onWorkspaceCommand(menu.session, "session.close", menu.scope) },
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
      aria-valuenow={Math.round(displayedWidth)}
      className="sidebar-resize"
      onKeyDown={(event) => {
        const delta = event.key === "ArrowLeft" ? -16 : event.key === "ArrowRight" ? 16 : 0;
        if (!delta) return;
        event.preventDefault();
        props.onWidth(displayedWidth + delta);
      }}
      onPointerDown={(event) => {
        const left = container.current?.getBoundingClientRect().left ?? 0;
        startWidthDrag(event, (pointer) => Math.max(
          SIDEBAR_MIN_WIDTH,
          Math.min(props.maxWidth, pointer.clientX - left),
        ));
      }}
      role="separator"
      tabIndex={0}
    />

    {agentMenu && sameHostConnection(agentMenu.scope, props.commandScope) && <ContextMenu
      anchor={agentMenu.anchor}
      items={agentMenu.row
        ? [
          { id: "focus", label: "Focus this agent's pane", disabled: !agentMenu.row.routable, run: () => props.onSelectAgent(agentMenu.row!, agentMenu.scope) },
          { id: "rename", label: "Rename agent…", disabled: !props.canMutate, run: () => props.onRenameAgent(agentMenu.row!.agent, agentMenu.scope) },
          ...resumePlacements(props.adapters, agentMenu.row.agent).map((placement) => ({
            id: `resume-${placement}`,
            label: `Resume in new ${placement}`,
            disabled: !props.canMutate,
            run: () => props.onResumeAgent(agentMenu.row!.agent, placement, agentMenu.scope),
          })),
        ]
        : launchItems(props.adapters, props.canMutate, props.onLaunchAgent, props.onReviewHooks, props.onSetUpHost)}
      label={agentMenu.row ? `Actions for ${agentMenu.row.agent.displayName}` : "Agent actions"}
      onClose={() => setAgentMenu(undefined)}
    />}
  </nav>;
}

/**
 * The compact row's workspace-level state, beside the workspace's name.
 *
 * Decorative on purpose: `rowLabel` below is the row's whole accessible name
 * and already names the loudest agent and its state, so this would be the
 * second announcement of one fact. `AgentStateIndicator` renders it
 * `aria-hidden` whenever no `label` is passed, which is why none is.
 */
function WorkspaceStateIndicator({ row, glyphs }: { row: WorkspaceRowModel; glyphs: boolean }) {
  const state = workspaceIndicatorState(row);
  if (!state) return <HiddenStateSlot />;
  return <AgentStateIndicator className="state-dot workspace-state" glyphs={glyphs} state={state} />;
}

/**
 * The indicator's footprint with nothing in it.
 *
 * A quiet workspace draws no mark, but dropping the element moved the name by
 * the dot plus the row's gap the moment an agent started working — and the
 * whole point of one indicator in one place is that the names do not move. The
 * stylesheet hides `.state-dot.idle` with `visibility`, so the box survives.
 */
function HiddenStateSlot() {
  return <span aria-hidden="true" className="state-dot workspace-state idle" />;
}

/**
 * The one state a workspace shows for itself: its loudest agent's.
 *
 * `row.attention` is already that — the rollup in `selectors.ts` combines by a
 * priority that runs blocked > done > working > unknown > idle > none, which is
 * the order this indicator wants and the same order the agents list sorts by.
 * All this adds is where the row goes quiet: idle, unknown and none render
 * nothing rather than a dot, because a workspace with nothing to say should not
 * put a mark beside its name to say so. Deriving a second ranking here would
 * mean a workspace row and the agent rows beneath it disagreeing about which
 * agent is the loudest one.
 */
function workspaceIndicatorState(row: WorkspaceRowModel): AgentDisplayState | undefined {
  switch (row.attention) {
    case "blocked": case "done": case "working": return row.attention;
    default: return undefined;
  }
}

/** One agent's line, written the same way for the eye and for the label. */
function agentLine(agent: WorkspaceRowModel["agents"][number]): string {
  return `${agent.name} · ${activityWord(agent.state)}`;
}

/**
 * The row's whole accessible name.
 *
 * It names the loudest agent and counts the rest rather than reading all four
 * lines: a busy workspace is one list item, and four announcements for one
 * item is how a list stops being navigable.
 */
function rowLabel(row: WorkspaceRowModel): string {
  const total = row.agents.length + row.agentOverflow;
  return [
    row.session.name,
    row.pinned ? "pinned" : undefined,
    row.agents[0] && agentLine(row.agents[0]),
    total > 1 ? `${total} agents` : undefined,
    row.unread > 0 ? `${row.unread} agent${row.unread === 1 ? "" : "s"} waiting` : undefined,
  ].filter(Boolean).join(", ");
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
  onSetUpHost?: () => void,
) {
  // Three optional sections, separated where two meet. `setUp` comes first and
  // only when there is something to set up: on a host that cannot report status
  // it is the only item anyone wants, and it is the way back for someone who
  // answered "not now".
  const sections = [
    onSetUpHost
      ? [{ id: "set-up-host", label: "Set up agent status on this host…", disabled: !canMutate, run: onSetUpHost }]
      : [],
    adapters
      .filter((adapter) => adapter.supportsLaunch)
      .flatMap((adapter) => adapter.placements.map((placement) => ({
        id: `launch-${adapter.id}-${placement}`,
        label: `New ${adapter.displayName} in a ${placement}`,
        disabled: !canMutate,
        run: () => onLaunch(adapter.id, placement),
      }))),
    adapters
      .filter((adapter) => adapter.supportsHooks)
      .flatMap((adapter) => (["install", "uninstall"] as const).map((action) => ({
        id: `hooks-${adapter.id}-${action}`,
        label: `${action === "install" ? "Review" : "Remove"} ${adapter.displayName} hooks…`,
        disabled: !canMutate,
        run: () => onReviewHooks(adapter.id, action),
      }))),
  ].filter((section) => section.length > 0);
  if (sections.length === 0) {
    return [{ id: "none", label: "No agent adapters available", disabled: true, run: () => undefined }];
  }
  return sections.flatMap((section, index) => index === 0 ? section : ["separator" as const, ...section]);
}

// @vitest-environment jsdom
import stylesCss from "../../styles.css?raw";
import { act as domAct } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot } from "react-dom/client";
import { act, create } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { HostProfile, Session } from "../../app/types";
import { rowCommandRegistry } from "../../commands/rowCommands";
import { agent } from "../agents/testFixtures";
import type { AgentAdapterDescriptor, AgentDisplayState } from "../agents/types";
import { buildAgentRows } from "../agents/agentsList";
import { HookReviewDialog } from "../agents/HookReviewDialog";
import { WorkspaceSidebar } from "../workspaces/WorkspaceSidebar";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../workspaces/TabStrip";
import { workspaceRows, type WorkspaceRowModel } from "../workspaces/workspaceRows";
import { deriveAgentRollups } from "../agents/selectors";
import { DisconnectedStrip, STRIP_APPEAR_DELAY_MS } from "./DisconnectedStrip";
import type { ConnectionPhase } from "../../state/connectionReducer";
import { RightPanel } from "./RightPanel";
import type { CombinedTab } from "./model";
import { defaultShellState, type ShellState } from "./types";
import { SettingsDialog } from "./SettingsDialog";
import { TitleBar } from "./TitleBar";

const noop = vi.fn();
const commandScope = { hostProfileId: "remote", connectionKey: "ssh:remote", connectionEpoch: 1, serverIdentity: "server-a", generation: 1 };
const session: Session = { id: "$1", name: "A very long workspace name", windowCount: 3, attachedClients: 1, order: 0 };
const rows: WorkspaceRowModel[] = [{
  session, active: true, attention: "blocked", unread: 2, working: true,
  agents: [{ id: "a1", adapterId: "codex", name: "codex", state: "blocked" }], agentOverflow: 0,
  branch: "main*", path: "~/dev/muxflow",
}];

/** One workspace holding exactly these agent states, through the real builder. */
function workspaceOf(...states: AgentDisplayState[]): WorkspaceRowModel[] {
  const agents = states.map((state, index) => agent({
    id: `a${index}`, sessionId: "$1", displayName: `agent ${index}`, updatedAt: states.length - index,
    ...(state === "done"
      ? { lifecycle: "idle" as const, attentionKind: "completed" as const, attentionGeneration: 4, seenGeneration: 1 }
      : { lifecycle: state }),
  }));
  return workspaceRows({
    snapshot: { sessions: [session], windows: [], panes: [] },
    activeSessionId: session.id,
    agents,
    attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
  });
}

/** What a compact row draws between its shortcut number and its name. */
function workspaceTitleIndicator(html: string): string {
  const title = html.slice(html.indexOf('class="workspace-title"'), html.indexOf('<span class="workspace-name"'));
  return title.slice(title.indexOf("</span>") + "</span>".length);
}

/** One workspace holding five agents, through the real row builder. */
function fiveAgentRows(): WorkspaceRowModel[] {
  const agents = [
    agent({ id: "a1", sessionId: "$1", displayName: "codex", lifecycle: "blocked", updatedAt: 5 }),
    agent({ id: "a2", sessionId: "$1", displayName: "claude", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 4, seenGeneration: 1, updatedAt: 4 }),
    agent({ id: "a3", sessionId: "$1", displayName: "aider", lifecycle: "working", updatedAt: 3 }),
    agent({ id: "a4", sessionId: "$1", displayName: "quiet one", lifecycle: "idle", updatedAt: 2 }),
    agent({ id: "a5", sessionId: "$1", displayName: "quiet two", lifecycle: "idle", updatedAt: 1 }),
  ];
  return workspaceRows({
    snapshot: { sessions: [session], windows: [], panes: [] },
    activeSessionId: session.id,
    agents,
    attentionByWorkspace: deriveAgentRollups(agents).byWorkspace,
    activeBranch: "main*",
  });
}

type SidebarProps = Parameters<typeof WorkspaceSidebar>[0];

const sidebarProps = (overrides: Partial<SidebarProps> = {}): SidebarProps => ({
  adapters: [],
  agents: buildAgentRows([agent({ displayName: "Codex one", windowName: "Review auth flow", lifecycle: "blocked" })], () => ({ workspaceOrder: 0, workspaceName: "work", hostLabel: "remote-linux", tabIndex: 1 }), () => true, "workspace"),
  agentSort: "workspace",
  agentsRatio: 0.4,
  compactWorkspaces: false,
  canMutate: true,
  commandScope,
  hostLabel: "remote-linux",
  latencyMs: 41,
  onAgentsRatio: noop,
  onLaunchAgent: noop,
  onOpenSettings: noop,
  onRenameAgent: noop,
  onResumeAgent: noop,
  onReviewHooks: noop,
  onSelectAgent: noop,
  onSelectWorkspace: noop,
  onSortMode: noop,
  onWorkspaceCommand: noop,
  phase: "connected",
  rows,
  maxWidth: 426,
  onWidth: noop,
  stateGlyphs: false,
  transport: "ssh",
  width: 240,
  ...overrides,
});

const sidebar = (overrides: Partial<SidebarProps> = {}) => renderToStaticMarkup(<WorkspaceSidebar {...sidebarProps(overrides)} />);

/**
 * Just the mark on the first agents-list row, out of a sidebar full of marks.
 *
 * The workspace rows above carry marks of their own, so a claim about one row's
 * badge has to be made against that row's slice or it is answered by somebody
 * else's badge. `agent-session-label` exists only on an agents-list row, and the
 * mark is what sits immediately before it.
 */
function agentRowMark(html: string): string {
  const label = html.indexOf('<span class="agent-session-label"');
  return html.slice(html.lastIndexOf('<span class="agent-mark"', label), label);
}

/**
 * The same sidebar in a real DOM, for the things that only exist there.
 *
 * The roving arrow keys resolve their next row by walking the rendered
 * document, which is the whole point of the fix they cover: a static string and
 * a react-test-renderer tree both hand back the element the component *thinks*
 * comes next, and the defect was that the component thought wrong.
 */
const domMounts: Array<{ root: ReturnType<typeof createRoot>; host: HTMLElement }> = [];

afterEach(async () => {
  while (domMounts.length) {
    const mounted = domMounts.pop()!;
    await domAct(async () => mounted.root.unmount());
    mounted.host.remove();
  }
});

async function mountSidebar(overrides: Partial<SidebarProps> = {}): Promise<HTMLElement> {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  domMounts.push({ root, host });
  await domAct(async () => root.render(<WorkspaceSidebar {...sidebarProps(overrides)} />));
  return host;
}

/** An arrow key on whatever currently has focus, through React's real listener. */
async function pressArrow(key: "ArrowUp" | "ArrowDown"): Promise<void> {
  const active = document.activeElement as HTMLElement | null;
  await domAct(async () => { active?.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key })); });
}

describe("application shell accessibility contracts", () => {
  it("renders one named sidebar holding both workspaces and agents", () => {
    const html = sidebar();
    expect(html).toContain('aria-label="Workspaces and agents"');
    expect(html).toContain('aria-current="true"');
    expect(html).toContain("A very long workspace name");
    expect(html).toContain("codex · blocked");
    // Neither the branch nor the working directory: a workspace holds many
    // tabs in many directories, so both are ⌘P's business alone now.
    expect(html).not.toContain('class="workspace-meta"');
    expect(html).not.toContain("main*");
    expect(html).not.toContain("~/dev/muxflow");
    // The one control in the agents header names both its state and its effect.
    // The persisted mode is still `status`; the word on the button is what the
    // mode does — blocked first, then working, then done, then idle.
    expect(html).toContain("Agent ordering: workspace. Switch to priority.");
    // The only resting connection indicator, and it is the way into settings.
    expect(html).toContain("Host remote-linux over ssh, connected. Open connection settings.");
    expect(html).toContain("41 ms");
  });

  it("badges only the workspaces and agents that are waiting on a human", () => {
    // The badge itself is decorative, so the count has to be in the row's own
    // accessible name or a screen reader never hears it.
    expect(sidebar()).toContain('aria-label="A very long workspace name, codex · blocked, 2 agents waiting"');
    expect(sidebar()).toContain("Review auth flow, blocked, waiting, work, remote-linux, tab 1");
    const quiet = sidebar({
      agents: buildAgentRows([agent({ displayName: "Claude", lifecycle: "working" })], () => ({ workspaceOrder: 0, workspaceName: "work" }), () => true, "workspace"),
      rows: [{ ...rows[0], unread: 0, attention: "working", working: true, agents: [], agentOverflow: 0 }],
    });
    expect(quiet).not.toContain("waiting");
    expect(quiet).not.toContain('class="badge badge-row"');
  });

  it("lists three agents on a workspace row and counts the rest", () => {
    // Built through `workspaceRows` from five real agents rather than from a
    // hand-written row, so the row model and the component cannot disagree
    // about which three get a line.
    const busy = sidebar({ rows: fiveAgentRows() });
    expect(busy.match(/class="workspace-activity-line/g)).toHaveLength(4);
    expect(busy).toContain("codex · blocked");
    expect(busy).toContain('data-agent-icon="codex"');
    expect(busy).toContain("claude · done, unread");
    expect(busy).toContain("aider · working");
    expect(busy).toContain("…2 more");
    // The line is a badged mark and the text, in that order — the state used
    // to be a separate dot in front of the icon and is now docked to it.
    expect(busy).toMatch(/class="agent-mark"><svg[^>]*data-agent-icon="codex"[\s\S]*?class="agent-mark-badge blocked"><\/span><\/span><span class="workspace-activity-text">codex · blocked/);
    // Four lines, one announcement: the label names the loudest and counts the
    // rest rather than reading every line of one list item.
    expect(busy).toContain('aria-label="A very long workspace name, codex · blocked, 5 agents, 2 agents waiting"');
    // Nothing to count means nothing is said about counting.
    expect(sidebar()).not.toContain("more");
  });

  it("keeps aggregate workspace status in compact mode while hiding agent lines", () => {
    const html = sidebar({ compactWorkspaces: true, rows: fiveAgentRows() });
    expect(html).not.toContain('class="workspace-activity"');
    expect(html).toContain('aria-label="A very long workspace name, codex · blocked, 5 agents, 2 agents waiting"');
    // The number is inside the title now, in front of the name — the row's
    // address rather than a column of digits beside it — and the workspace's
    // own state sits between the two. This workspace holds a blocked agent, so
    // that is what the row says about itself; the spinner belongs to the
    // cluster's working mark and not to the title.
    expect(html).toContain('<span class="workspace-title"><span aria-hidden="true" class="workspace-shortcut-index">1</span><span aria-hidden="true" class="state-dot workspace-state blocked">!</span><span class="workspace-name">');
    expect(html).not.toContain('class="spinner"');
    expect(html).toContain('class="spinner agent-mark-badge working"');
    // The non-compact row has no cluster to carry it, so it keeps the spinner.
    expect(sidebar({ rows: fiveAgentRows() }))
      .toContain('<span aria-hidden="true" class="spinner"></span><span class="workspace-name">');
    // A cluster of badged marks stands in for the three agent lines: three at
    // most, then a mono overflow count. Decorative — `rowLabel` above is the
    // whole accessible name.
    const cluster = html.slice(html.indexOf('class="workspace-agents"'), html.indexOf('class="badge badge-inline"'));
    expect(cluster.match(/class="agent-mark"/g)).toHaveLength(3);
    expect(cluster).toContain('class="workspace-agents-more">+2</span>');
    // Inline at the far right of the one line, not floated over it.
    expect(html).toContain('class="badge badge-inline">2');
    expect(html.slice(0, html.indexOf("section-divider"))).not.toContain('class="badge badge-row"');
  });

  it("gives a compact row one workspace-level indicator for its loudest state", () => {
    // The bug this covers: a compact row said nothing about the workspace at
    // all. The only marks on it were the per-agent cluster out at the right
    // edge, so "this workspace is blocked" had to be read off a 6px badge on
    // one of three icons — while the non-compact row had carried a title
    // spinner all along. One indicator, at the loudest state, in the title.
    const indicator = (...states: AgentDisplayState[]) =>
      workspaceTitleIndicator(sidebar({ compactWorkspaces: true, rows: workspaceOf(...states) }));
    // Blocked outranks everything, done outranks working, and each of the three
    // is asserted against a workspace that also holds the states it beats — the
    // ranking is the point, not the single-agent case.
    expect(indicator("blocked", "done", "working", "idle")).toBe('<span aria-hidden="true" class="state-dot workspace-state blocked">!</span>');
    expect(indicator("done", "working", "idle")).toBe('<span aria-hidden="true" class="state-dot workspace-state done">✓</span>');
    // Working is a process, not a condition, so it spins — the same 9px title
    // spinner the non-compact row draws, in the same place, inheriting the ink
    // of whichever row it is on.
    expect(indicator("working", "idle", "unknown")).toBe('<span aria-hidden="true" class="spinner"></span>');
    // Nothing to say, nothing drawn — but the slot stays, hidden, so the name
    // beside it does not slide over the moment an agent starts working. A dot
    // beside the name is "look here", and a workspace of idle agents is the
    // resting state of every quiet row in a long list.
    const quiet = '<span aria-hidden="true" class="state-dot workspace-state idle"></span>';
    expect(indicator("idle")).toBe(quiet);
    expect(indicator("unknown")).toBe(quiet);
    expect(indicator()).toBe(quiet);
    // With the glyph option on the state stops depending on colour, working
    // included — and the quiet states stay quiet, since a shape for "nothing is
    // happening" is still a mark on a row that has nothing to report.
    const glyphed = (...states: AgentDisplayState[]) =>
      workspaceTitleIndicator(sidebar({ compactWorkspaces: true, stateGlyphs: true, rows: workspaceOf(...states) }));
    expect(glyphed("blocked")).toBe('<span aria-hidden="true" class="state-dot workspace-state blocked glyphs">!</span>');
    expect(glyphed("working")).toBe('<span aria-hidden="true" class="state-dot workspace-state working glyphs">•</span>');
    expect(glyphed("idle")).toBe(quiet);
    // Non-compact keeps its own rule: its agent lines report blocked and done in
    // words, so the title spins for working alone. It obeys the same no-shift
    // rule though — a row that is not working holds the same hidden slot.
    const wide = (...states: AgentDisplayState[]) => workspaceTitleIndicator(sidebar({ rows: workspaceOf(...states) }));
    expect(wide("working")).toBe('<span aria-hidden="true" class="spinner"></span>');
    expect(wide("blocked")).toBe(quiet);
    expect(wide("blocked", "working")).toBe('<span aria-hidden="true" class="spinner"></span>');
    // The indicator is decorative in both modes: `rowLabel` is the row's whole
    // accessible name and already names the loudest agent and its state.
    expect(sidebar({ compactWorkspaces: true, rows: workspaceOf("blocked") }))
      .toContain('aria-label="A very long workspace name, agent 0 · blocked, 1 agent waiting"');

    // A done workspace takes the bright notification green the badges and the
    // tab strip use, not the muted teal the static state palette paints. Both
    // fills sit within about 1.5:1 of the accent block, so the active row rings
    // them in the knockout ink rather than leaving them to dissolve into it.
    expect(stylesCss).toContain(".state-dot.workspace-state.done { background: var(--ok); }");
    expect(stylesCss).toContain(".workspace-button.active .state-dot.workspace-state { box-shadow: 0 0 0 1.5px var(--accent-ink); }");
    // Working and done are one indicator a state apart, so the spinner is
    // sized to the dot it stands in for; the plain 9px spinner moved the
    // workspace name by a pixel every time an agent finished.
    expect(stylesCss).toContain(".workspace-title .spinner { width: 8px; height: 8px; }");
    // And under forced colors, where the dot grows to 11px to hold a glyph, so
    // does the spinner that replaces it — otherwise the invariant holds in the
    // default mode only and the name slides 3px there on every state change.
    const forcedColors = stylesCss.slice(stylesCss.indexOf("@media (forced-colors: active)"));
    expect(forcedColors).toContain(".workspace-title .spinner { width: 11px; height: 11px; }");
  });

  it("nests workspace-ordered agents under host-qualified headings in keyboard order", () => {
    const source = [
      agent({ id: "one", windowName: "Plan rollout", sessionId: "$1", sessionName: "api" }),
      agent({ id: "two", windowName: "Fix tests", sessionId: "$2", sessionName: "web", adapterId: "claude-code" }),
    ];
    const agents = buildAgentRows(source, (record) => ({
      workspaceOrder: record.sessionId === "$1" ? 0 : 1,
      workspaceName: record.sessionName,
      hostLabel: "remote-linux",
    }), () => true, "workspace");
    const html = sidebar({ agents });
    expect(html).toContain('class="agent-workspace-heading"');
    // The host stays in the tooltip — one connection is live at a time, so on
    // the line it was the same word on every heading — and the line's right
    // edge carries the group's size instead.
    expect(html).toContain('title="api · remote-linux"');
    expect(html).toContain('title="web · remote-linux"');
    expect(html).not.toContain('class="agent-workspace-host"');
    expect(html).toContain('<span>api</span><span aria-hidden="true" class="agent-group-count">1</span>');
    expect(html.indexOf('data-agent-index="0"')).toBeLessThan(html.indexOf('data-agent-index="1"'));
    expect(html).toContain('data-agent-icon="codex"');
    expect(html).toContain('data-agent-icon="claude"');
    expect(html).toContain('<span class="agent-session-label">Plan rollout</span><span class="agent-detail">');
  });

  /**
   * All four buckets populated, done and working both present.
   *
   * That last part is the whole fixture: `compareAgents` ranks done-unread
   * above working, and the headings read Blocked → Working → Done → Idle, so
   * this is the one shape in which the flat index order and the reading order
   * disagree. A fixture without a done agent agrees with itself and proves
   * nothing about either.
   */
  const priorityAgents = () => buildAgentRows([
    agent({ id: "b", windowName: "Fix the build", sessionId: "$1", sessionName: "api", lifecycle: "blocked", updatedAt: 5 }),
    agent({ id: "w", windowName: "Run the suite", sessionId: "$2", sessionName: "web", lifecycle: "working", updatedAt: 4 }),
    agent({ id: "d", windowName: "Ship the patch", sessionId: "$2", sessionName: "web", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 4, seenGeneration: 1, updatedAt: 3 }),
    agent({ id: "u", windowName: "Never reported", sessionId: "$2", sessionName: "web", lifecycle: "unknown", updatedAt: 2 }),
    agent({ id: "i", windowName: "Nothing doing", sessionId: "$2", sessionName: "web", lifecycle: "idle", updatedAt: 1 }),
  ], (record) => ({ workspaceOrder: 0, workspaceName: record.sessionName }), () => true, "status");

  it("draws the priority order as real groups, keyed on the flat keyboard index", () => {
    // The sort put blocked above working above idle and left it at that, so a
    // list of twelve read as one undifferentiated column. Same clustering the
    // workspace mode uses, keyed on what the agent is doing.
    const html = sidebar({ agentSort: "status", agents: priorityAgents() });
    expect(html).toContain(">priority</button>");
    for (const label of ["Blocked", "Working", "Done", "Idle"]) expect(html, label).toContain(`<span>${label}</span>`);
    // Blocked, working, done, idle — the reading order, and deliberately not
    // the sort's done-outranks-working ranking, which is what puts Working
    // between Blocked and Done here.
    expect(html.indexOf(">Blocked<")).toBeLessThan(html.indexOf(">Working<"));
    expect(html.indexOf(">Working<")).toBeLessThan(html.indexOf(">Done<"));
    expect(html.indexOf(">Done<")).toBeLessThan(html.indexOf(">Idle<"));
    // Unknown shares Idle's group rather than earning a fifth heading, so that
    // group counts two.
    expect(html).toContain('<span>Idle</span><span aria-hidden="true" class="agent-group-count">2</span>');
    // The workspace is in each row's detail, because the grouping no longer
    // says it.
    expect(html).toContain("web · working");
    // One flat index across every group — a per-group index would restart the
    // roving walk at every heading — and it is *not* ascending in the document,
    // because done sorts above working and reads below it. Which is exactly why
    // `focusRelative` cannot treat the number as a document position.
    expect([...html.matchAll(/data-agent-index="(\d+)"/g)].map((match) => match[1])).toEqual(["0", "2", "1", "3", "4"]);
  });

  it("walks every priority row exactly once with the arrow keys, across the group seams", async () => {
    // `focusRelative` used to step the flat `data-agent-index` and then take
    // the query result at that position: two different orders walked at once.
    // With a done and a working agent both present, going down skipped the Done
    // row and stuck at the bottom, and coming back up skipped the Working one.
    const host = await mountSidebar({ agentSort: "status", agents: priorityAgents() });
    expect([...host.querySelectorAll<HTMLElement>(".agent-workspace-heading")].map((node) => node.id))
      .toEqual(["agent-status-blocked", "agent-status-working", "agent-status-done", "agent-status-idle"]);

    const walkRows = [...host.querySelectorAll<HTMLElement>("[data-agent-index]")];
    expect(walkRows.map((row) => row.querySelector(".agent-session-label")?.textContent))
      .toEqual(["Fix the build", "Run the suite", "Ship the patch", "Never reported", "Nothing doing"]);
    // The identity on each row is the flat index, and it does not ascend.
    expect(walkRows.map((row) => row.dataset.agentIndex)).toEqual(["0", "2", "1", "3", "4"]);

    walkRows[0].focus();
    const down = [document.activeElement];
    for (let step = 1; step < walkRows.length; step += 1) { await pressArrow("ArrowDown"); down.push(document.activeElement); }
    expect(down).toEqual(walkRows);
    // The end of the list is the end, not a wrap onto the top.
    await pressArrow("ArrowDown");
    expect(document.activeElement).toBe(walkRows.at(-1));

    const up = [document.activeElement];
    for (let step = 1; step < walkRows.length; step += 1) { await pressArrow("ArrowUp"); up.push(document.activeElement); }
    expect(up).toEqual([...walkRows].reverse());
    await pressArrow("ArrowUp");
    expect(document.activeElement).toBe(walkRows[0]);
  });

  it("walks the workspace rows the same way", async () => {
    // Same lookup, for uniformity: the workspace indexes happen to ascend in
    // the document today, and the walk should not be the thing that depends on
    // it staying that way.
    const host = await mountSidebar({
      rows: [0, 1, 2].map((index) => ({
        ...rows[0],
        session: { ...session, id: `$${index + 1}`, name: `work ${index + 1}`, order: index },
        active: index === 0,
      })),
    });
    const workspaceRowNodes = [...host.querySelectorAll<HTMLElement>("[data-workspace-index]")];
    expect(workspaceRowNodes).toHaveLength(3);
    workspaceRowNodes[0].focus();
    const visited = [document.activeElement];
    for (let step = 1; step < workspaceRowNodes.length; step += 1) { await pressArrow("ArrowDown"); visited.push(document.activeElement); }
    expect(visited).toEqual(workspaceRowNodes);
    await pressArrow("ArrowUp");
    expect(document.activeElement).toBe(workspaceRowNodes[1]);
  });

  it("reaches the agent row actions from the command registry, on the last agent focused", async () => {
    const onSelectAgent = vi.fn();
    const onRenameAgent = vi.fn();
    const onResumeAgent = vi.fn();
    const agents = buildAgentRows(
      [agent({ id: "a1", displayName: "Codex one", lifecycle: "blocked" })],
      () => ({ workspaceOrder: 0, workspaceName: "work", tabIndex: 1 }), () => true, "workspace",
    );
    const adapters: AgentAdapterDescriptor[] = [{
      id: "codex", displayName: "Codex", supportsLaunch: true, supportsResume: true, supportsHooks: true,
      supportsProcessDetection: true, hookConfigPath: "~/.codex/config.toml",
      hookEvents: [], placements: ["window", "split"], hookWiring: "wired", hookWiringDetail: "", hookSetupRecommended: false,
    }];
    let renderer!: ReturnType<typeof create>;
    const element = (rows: typeof agents, canMutate = true) => <WorkspaceSidebar
      adapters={adapters} agents={rows} agentSort="workspace" agentsRatio={0.4} canMutate={canMutate} commandScope={commandScope} compactWorkspaces={false}
      hostLabel="remote-linux" latencyMs={41} maxWidth={426} phase="connected" rows={[]} stateGlyphs={false} transport="ssh" width={240}
      onAgentsRatio={noop} onLaunchAgent={noop} onOpenSettings={noop} onRenameAgent={onRenameAgent}
      onResumeAgent={onResumeAgent} onReviewHooks={noop} onSelectAgent={onSelectAgent} onSelectWorkspace={noop}
      onSortMode={noop} onWidth={noop} onWorkspaceCommand={noop}
    />;
    await act(async () => { renderer = create(element(agents)); });
    // No agent focused: nothing to act on.
    expect(rowCommandRegistry.available()).toEqual([]);

    const row = renderer.root.findAllByProps({ className: "agent-button" })[0];
    await act(async () => { row.props.onFocus(); });
    expect(rowCommandRegistry.available()).toEqual(["agents.focusRow", "agents.renameRow", "agents.resumeRow"]);
    await act(async () => { rowCommandRegistry.run("agents.resumeRow"); });
    // One line in the palette takes the adapter's first declared placement; the
    // context menu is where a choice between placements belongs.
    expect(onResumeAgent).toHaveBeenCalledWith(expect.objectContaining({ id: "a1" }), "window", commandScope);
    await act(async () => { rowCommandRegistry.run("agents.focusRow"); });
    expect(onSelectAgent).toHaveBeenCalled();

    // The agent's pane closed while it was the palette's subject: the row goes
    // and its commands go with it, rather than acting on a stale record.
    await act(async () => { renderer.update(element([])); });
    expect(rowCommandRegistry.available()).toEqual([]);
    await act(async () => { renderer.unmount(); });
  });

  it("encodes every agent state in the mark's class, which is where the color comes from", () => {
    // The badge has no background of its own: `.agent-mark-badge.blocked` and
    // friends carry it. A badge rendered as a bare `agent-mark-badge` is an
    // invisible 6px box, and the agents section loses the whole encoding the
    // mock is built around — which is exactly what shipped when the dot's class
    // stopped interpolating, one indicator ago.
    const row = (lifecycle: "working" | "blocked" | "idle", stateGlyphs = false) => sidebar({
      stateGlyphs,
      agents: buildAgentRows([agent({ displayName: "A", lifecycle })], () => ({ workspaceOrder: 0, workspaceName: "work" }), () => true, "workspace"),
    });
    expect(row("blocked")).toContain('class="agent-mark-badge blocked"');
    // Idle is the resting state and docks nothing: a badge means "look here",
    // and an agent nobody is waiting on must not compete for the eye. The
    // fixture's workspace section has a blocked agent of its own, so the claim
    // is made on the idle agent's own mark rather than on the whole document.
    expect(agentRowMark(row("idle"))).not.toContain("agent-mark-badge");
    expect(agentRowMark(row("blocked"))).toContain("agent-mark-badge blocked");
    // Working is the one state that is a process rather than a condition, so
    // it spins — on the badge here, the same way it spins on a tab.
    expect(row("working")).toContain('class="spinner agent-mark-badge working"');
    expect(row("working")).not.toContain('class="agent-mark-badge working"');
    // With the glyph option on the badge steps aside: 6px cannot hold a
    // legible "!" or "✓", and the option exists so state does not depend on
    // colour. Full-size dots come back, working included.
    expect(row("working", true)).toContain('class="state-dot working glyphs"');
    expect(row("blocked", true)).toContain('class="state-dot blocked glyphs"');
    expect(row("blocked", true)).not.toContain("agent-mark-badge");
    // Every state the list can produce must have a rule to match, or the same
    // defect returns for one state instead of all of them. Idle renders no
    // badge at all, so it needs no badge rule — only its glyph-mode dot.
    // Read the same way `theme.test.ts` reads `tokens.css`: the real file.
    for (const state of ["working", "blocked", "done", "unknown", "idle"]) {
      expect(stylesCss, state).toContain(`.state-dot.${state}`);
    }
    for (const state of ["working", "blocked", "done", "unknown"]) {
      expect(stylesCss, state).toContain(`.agent-mark-badge.${state}`);
    }
    expect(stylesCss).not.toContain(".agent-mark-badge.idle");
    // Spinners are one neutral ink everywhere — motion says "working"; the
    // state colours stay reserved for the static dots — and a done badge is a
    // notification, so it takes the bright green rather than the muted teal.
    expect(stylesCss).toContain(".agent-mark-badge.working { width: 7px; height: 7px; background: var(--badge-ring); color: var(--chrome-ink); }");
    expect(stylesCss).toContain(".workspace-button.active .agent-mark-badge.working { color: var(--accent-ink); }");
    expect(stylesCss).toContain(".agent-mark-badge.done { background: var(--ok); }");
    expect(stylesCss).toContain(".tab-agent-spinner { width: 8px; height: 8px; color: var(--chrome-ink); }");
    // The ring is the row's own background punched out around the badge, and
    // every row background it can sit on has to say which one it is.
    expect(stylesCss).toContain("--badge-ring: var(--chrome-bg)");
    expect(stylesCss).toContain(".workspace-button.active .agent-mark { --badge-ring: var(--accent); }");
    // Unknown is the one badge whose colour lives in a border, and on the
    // active row that border is drawn against the accent block, where
    // --state-unknown falls to roughly 2:1. It flips to the knockout ink
    // there, like the shortcut index and the icons already do.
    expect(stylesCss).toContain(".workspace-button.active .agent-mark-badge.unknown { border-color: color-mix(in srgb, var(--accent-ink) 65%, transparent); }");
    // One size for every group-heading indicator. `.state-dot` is a single
    // class and comes later in the file, so 7px has to win on specificity or
    // the non-Working headings quietly draw an 8px dot next to a 7px spinner.
    expect(stylesCss).toContain(".state-dot.agent-group-dot, .spinner.agent-group-dot { width: 7px; height: 7px;");
    // With the glyph option on the mark is still one node, and the gap between
    // its dot and its icon is its own — tighter than the gap the compact
    // cluster puts between two agents.
    expect(stylesCss).toContain(".agent-mark-glyphs { align-items: center; gap: 3px; }");
    expect(stylesCss).toContain(".workspace-agents { display: flex; align-items: center; gap: 7px;");
  });

  it("keeps the compact cluster one node per agent with the glyph option on", () => {
    // A two-node fragment put the dot and the icon straight into the cluster's
    // flex row, which has one uniform gap: dot, icon, dot, icon at 7px apiece,
    // no visible pairing, and three agents taking the width of six marks.
    const html = sidebar({ compactWorkspaces: true, rows: fiveAgentRows(), stateGlyphs: true });
    const cluster = html.slice(html.indexOf('class="workspace-agents"'), html.indexOf('class="badge badge-inline"'));
    expect(cluster.match(/class="agent-mark agent-mark-glyphs"/g)).toHaveLength(3);
    expect(cluster.match(/class="agent-icon/g)).toHaveLength(3);
    expect(cluster.match(/class="state-dot /g)).toHaveLength(3);
    // Every dot is inside a wrapper, so the cluster's own children are the
    // three agents and the overflow count and nothing else.
    expect(cluster).not.toMatch(/class="workspace-agents"><span class="state-dot/);
  });

  it("states empty sidebar sections in one line each", () => {
    const html = sidebar({ rows: [], agents: [] });
    expect(html).toContain("No tmux sessions on this host yet.");
    expect(html).toContain("No agents detected.");
  });

  it("renders combined terminal/app tabs as one selected tablist", () => {
    const html = renderToStaticMarkup(<TabStrip
      activeKey="app:file" activeTerminalPaneCount={1} canMutate canSplit commandScope={commandScope} stateGlyphs={false} onClose={noop}
      onCloseCurrent={noop} onCloseNonAgent={noop} onCloseOthers={noop} onCloseRight={noop} onDownloadTab={noop} onMove={noop}
      onNewTerminal={noop} onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={[
        { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "blocked", agentPresence: "present" },
        { key: "app:file", kind: "app", id: "file", title: "README.md", appKind: "markdown", resource: "/r/README.md", order: 0, preview: true, canMoveLeft: false, canMoveRight: false },
      ]}
    />);
    expect(html).toContain('role="tablist"');
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain(`id="${workspaceTabDomId("app:file")}"`);
    expect(html).toContain(`aria-controls="${workspaceTabPanelDomId("app:file")}"`);
    expect(html).toContain("README.md");
    // A document tab closes in place; a terminal tab does not, because closing
    // a tmux window destroys live processes and goes through confirmation.
    expect(html).toContain("Close README.md");
    expect(html).not.toContain("Close shell");
    expect(html).toContain('aria-label="Agent blocked"');
    // A preview tab says so in the strip, the way VS Code does.
    expect(html).toContain('class="tab-title tab-title-preview"');
  });

  it("shows positional shortcut numbers on only the first nine workspaces and tabs", () => {
    const manyRows = Array.from({ length: 10 }, (_, index): WorkspaceRowModel => ({
      ...rows[0],
      session: { ...session, id: `$${index + 1}`, name: `workspace-${index + 1}`, order: index },
      active: index === 0,
      attention: "none",
      unread: 0,
      working: false,
      agents: [],
    }));
    const workspaceHtml = sidebar({ rows: manyRows });
    expect(workspaceHtml.match(/class="workspace-shortcut-index"/g)).toHaveLength(9);
    expect(workspaceHtml).toContain('class="workspace-shortcut-index">9</span>');
    // Inside the title, in front of the name: the number addresses the row, and
    // on its own line it read as a column of digits parallel to the names.
    expect(workspaceHtml).toContain('<span class="workspace-title"><span aria-hidden="true" class="workspace-shortcut-index">9</span>');

    const manyTabs = Array.from({ length: 10 }, (_, index) => ({
      key: `app:file-${index}` as const,
      kind: "app" as const,
      id: `file-${index}`,
      title: `file-${index}.ts`,
      appKind: "file" as const,
      resource: `/r/file-${index}.ts`,
      order: index,
      preview: false,
      canMoveLeft: index > 0,
      canMoveRight: index < 9,
    }));
    const tabHtml = renderToStaticMarkup(<TabStrip
      activeKey={manyTabs[0].key} activeTerminalPaneCount={0} canMutate canSplit={false}
      commandScope={commandScope} stateGlyphs={false} onClose={noop} onCloseCurrent={noop}
      onCloseNonAgent={noop} onCloseOthers={noop} onCloseRight={noop} onDownloadTab={noop}
      onMove={noop} onNewTerminal={noop} onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={[...manyTabs, { key: "pending:create", kind: "pending", title: "Creating" }]}
    />);
    expect(tabHtml.match(/class="tab-index"/g)).toHaveLength(9);
    expect(tabHtml).toContain('class="tab-index">9</span>');
  });

  it("renders a working spinner and distinct blocked and unread-complete tab marks, and nothing for idle", () => {
    const states = ["working", "blocked", "done", "idle"] as const;
    const html = renderToStaticMarkup(<TabStrip
      activeKey="terminal:@1" activeTerminalPaneCount={1} canMutate canSplit commandScope={commandScope}
      stateGlyphs={false} onClose={noop} onCloseCurrent={noop} onCloseNonAgent={noop} onCloseOthers={noop}
      onCloseRight={noop} onDownloadTab={noop} onMove={noop} onNewTerminal={noop} onPin={noop}
      onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={states.map((attention, index) => ({
        key: `terminal:@${index + 1}` as const, kind: "terminal" as const, id: `@${index + 1}`,
        title: attention, index: index + 1, activeInTmux: index === 0, zoomed: false,
        canMoveLeft: index > 0, canMoveRight: index < states.length - 1, attention, agentPresence: "present" as const,
      }))}
    />);
    expect(html).toContain('class="spinner tab-agent-spinner"');
    expect(html).not.toContain('class="tab-dot working"');
    expect(html).toContain('class="tab-dot blocked"');
    expect(html).toContain('class="tab-dot done"');
    // Idle is the resting state and shows no mark — the rule the sidebar's
    // marks already follow. What it does keep is the slot: an empty, hidden,
    // decorative span, so the tab is exactly as wide idle as it is working and
    // the title does not jump when an agent starts or finishes.
    expect(html).toContain('<span aria-hidden="true" class="tab-dot idle"></span>');
    expect(html).not.toContain('aria-label="Agent idle"');
    expect(stylesCss).toContain(".state-dot.idle, .state-dot.none, .tab-dot.idle { visibility: hidden; }");
    // The reserved slot is only worth anything if the two marks that can fill
    // it are the same size, so the dot is sized to the spinner rather than to
    // the 6px it used to be.
    expect(stylesCss).toContain(".tab-agent-spinner { width: 8px; height: 8px;");
    expect(stylesCss).toMatch(/\.tab-dot \{[^}]*width: 8px; height: 8px;/);
    // Including under forced colors, where the dot grows to hold a glyph: the
    // spinner has to grow with it or the invariant holds in one mode only.
    const forced = stylesCss.slice(stylesCss.indexOf("@media (forced-colors: active)"));
    expect(forced).toMatch(/\.state-dot, \.tab-dot \{[^}]*width: 11px; height: 11px;/);
    expect(forced).toContain(".tab-agent-spinner { width: 11px; height: 11px; }");
    expect(stylesCss).toContain(".tab-dot.done { background: var(--ok); }");
    expect(stylesCss).toContain("@media (prefers-reduced-motion: no-preference)");
  });

  it("names the agent in a terminal tab with its adapter mark, and only when there is one", () => {
    const terminal = (id: string, extra: Partial<Extract<CombinedTab, { kind: "terminal" }>>) => ({
      key: `terminal:${id}` as const, kind: "terminal" as const, id, title: `window ${id}`,
      index: 1, activeInTmux: false, zoomed: false, canMoveLeft: false, canMoveRight: false,
      attention: "none" as const, agentPresence: "absent" as const, ...extra,
    });
    const html = renderToStaticMarkup(<TabStrip
      activeKey="terminal:@1" activeTerminalPaneCount={1} canMutate canSplit commandScope={commandScope}
      stateGlyphs={false} onClose={noop} onCloseCurrent={noop} onCloseNonAgent={noop} onCloseOthers={noop}
      onCloseRight={noop} onDownloadTab={noop} onMove={noop} onNewTerminal={noop} onPin={noop}
      onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={[
        terminal("@1", { attention: "working", agentAdapterId: "codex", agentPresence: "present" }),
        terminal("@2", {}),
        // The rollup names an adapter only where it found an agent, so the id
        // is the evidence. Presence is a coarser, app-wide answer that turns
        // "unknown" for a round trip after any topology change — gating the
        // mark on it blinked the icon off every tab at once.
        terminal("@3", { agentAdapterId: "codex", agentPresence: "unknown" }),
      ]}
    />);
    // Identity in the glyph slot, state in the dot — the same separation the
    // sidebar keeps. Decorative: the title and the indicator carry the meaning.
    expect(html).toMatch(/data-agent-icon="codex"[\s\S]*?<span class="tab-title">window @1<\/span>/);
    expect(html).toContain('aria-hidden="true" class="agent-icon codex"');
    expect(html).toContain('class="spinner tab-agent-spinner"');
    // Two marks for the two tabs the rollup named an adapter for, and with both
    // falling before @1's and @3's titles, none is left for @2.
    expect(html.match(/data-agent-icon=/g)).toHaveLength(2);
    expect(html).toMatch(/data-agent-icon="codex"[\s\S]*?window @1[\s\S]*?data-agent-icon="codex"[\s\S]*?window @3/);
    expect(stylesCss).toContain(".tab-select .agent-icon { color: inherit; }");
  });

  it("keeps a tab menu command bound to the connection scope that opened it", async () => {
    const onClose = vi.fn();
    const tab = { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "none", agentPresence: "absent" } as const;
    const replacementScope = { ...commandScope, connectionEpoch: 2, serverIdentity: "server-b" };
    const element = (scope: typeof commandScope) => <TabStrip
      activeKey={tab.key} activeTerminalPaneCount={1} canMutate canSplit commandScope={scope} stateGlyphs={false} onClose={onClose}
      onCloseCurrent={noop} onCloseNonAgent={noop} onCloseOthers={noop} onCloseRight={noop} onDownloadTab={noop} onMove={noop}
      onNewTerminal={noop} onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop} tabs={[tab]}
    />;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(commandScope)); });
    await act(async () => renderer.root.findByProps({ role: "tab" }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    await act(async () => { renderer.update(element(replacementScope)); });
    await act(async () => renderer.root.findByProps({ "data-menu-item": "close" }).props.onClick());
    expect(onClose).toHaveBeenCalledWith(tab, commandScope);
    await act(async () => renderer.unmount());
  });

  it("makes the active split tab's menu Close use the same pane-first command as the keyboard", async () => {
    const onClose = vi.fn();
    const onCloseCurrent = vi.fn();
    const tab = { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "none", agentPresence: "absent" } as const;
    const replacementScope = { ...commandScope, connectionEpoch: 2, serverIdentity: "server-b" };
    const element = (scope: typeof commandScope, paneId: string) => <TabStrip
      activeKey={tab.key} activePaneId={paneId} activeTerminalPaneCount={2} canMutate canSplit commandScope={scope}
      stateGlyphs={false} onClose={onClose} onCloseCurrent={onCloseCurrent} onCloseNonAgent={noop}
      onCloseOthers={noop} onCloseRight={noop} onDownloadTab={noop} onMove={noop} onNewTerminal={noop}
      onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop} tabs={[tab]}
    />;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(commandScope, "%1")); });
    await act(async () => renderer.root.findByProps({ role: "tab" }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    await act(async () => renderer.update(element(replacementScope, "%recycled")));
    const close = renderer.root.findByProps({ "data-menu-item": "close" });
    expect(close.findByType("span").children.join("")).toBe("Close Pane");
    await act(async () => close.props.onClick());
    expect(onCloseCurrent).toHaveBeenCalledWith("%1", commandScope);
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("keeps an open tab menu's close label and action aligned after external tab activation", async () => {
    const onClose = vi.fn();
    const onCloseCurrent = vi.fn();
    const tab = { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "none", agentPresence: "absent" } as const;
    const element = (activeKey: string) => <TabStrip
      activeKey={activeKey} activePaneId="%1" activeTerminalPaneCount={2} canMutate canSplit commandScope={commandScope}
      stateGlyphs={false} onClose={onClose} onCloseCurrent={onCloseCurrent} onCloseNonAgent={noop}
      onCloseOthers={noop} onCloseRight={noop} onDownloadTab={noop} onMove={noop} onNewTerminal={noop}
      onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop} tabs={[tab]}
    />;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(tab.key)); });
    await act(async () => renderer.root.findByProps({ role: "tab" }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    await act(async () => renderer.update(element("terminal:@2")));
    const close = renderer.root.findByProps({ "data-menu-item": "close" });
    expect(close.findByType("span").children.join("")).toBe("Close tab…");
    await act(async () => close.props.onClick());
    expect(onClose).toHaveBeenCalledWith(tab, commandScope);
    expect(onCloseCurrent).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("closes an active app tab instead of its covered terminal pane", async () => {
    const onClose = vi.fn();
    const onCloseCurrent = vi.fn();
    const tab = {
      key: "app:file", kind: "app", id: "file", title: "README.md", appKind: "markdown",
      resource: "/r/README.md", order: 0, preview: false, canMoveLeft: false, canMoveRight: false,
    } as const;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<TabStrip
      activeKey={tab.key} activePaneId="%covered" activeTerminalPaneCount={2} canMutate canSplit={false}
      commandScope={commandScope} stateGlyphs={false} onClose={onClose} onCloseCurrent={onCloseCurrent}
      onCloseNonAgent={noop} onCloseOthers={noop} onCloseRight={noop} onDownloadTab={noop} onMove={noop}
      onNewTerminal={noop} onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop} tabs={[tab]}
    />); });
    await act(async () => renderer.root.findByProps({ role: "tab" }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    await act(async () => renderer.root.findByProps({ "data-menu-item": "close" }).props.onClick());
    expect(onClose).toHaveBeenCalledWith(tab, commandScope);
    expect(onCloseCurrent).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("offers the bulk closes and Download only where they have something to do", async () => {
    const onCloseOthers = vi.fn();
    const onCloseRight = vi.fn();
    const onDownloadTab = vi.fn();
    const onCloseNonAgent = vi.fn();
    const strip = [
      { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: true, attention: "none", agentPresence: "absent" },
      { key: "terminal:@2", kind: "terminal", id: "@2", title: "logs", index: 2, activeInTmux: false, zoomed: false, canMoveLeft: true, canMoveRight: false, attention: "none", agentPresence: "absent" },
      { key: "app:file", kind: "app", id: "file", title: "README.md", appKind: "markdown", resource: "/r/README.md", order: 0, preview: false, canMoveLeft: false, canMoveRight: true },
      { key: "app:diff", kind: "app", id: "diff", title: "a.ts (staged)", appKind: "gitDiff", resource: "staged:a.ts", order: 1, preview: false, canMoveLeft: true, canMoveRight: false },
    ] as const;
    const element = (canMutate: boolean) => <TabStrip
      activeKey="app:file" activeTerminalPaneCount={1} canMutate={canMutate} canSplit commandScope={commandScope} stateGlyphs={false}
      onClose={noop} onCloseCurrent={noop} onCloseNonAgent={onCloseNonAgent} onCloseOthers={onCloseOthers} onCloseRight={onCloseRight} onDownloadTab={onDownloadTab}
      onMove={noop} onNewTerminal={noop} onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={[...strip]}
    />;
    const openMenuOn = async (renderer: ReturnType<typeof create>, index: number) => {
      await act(async () => renderer.root.findAllByProps({ role: "tab" })[index]
        .props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    };
    const item = (renderer: ReturnType<typeof create>, id: string) => renderer.root.findAllByProps({ "data-menu-item": id });

    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(true)); });
    // A file tab in the middle: three others behind it, one tab to its right,
    // and a file to save.
    await openMenuOn(renderer, 2);
    expect(item(renderer, "closeOthers")[0].props.disabled).toBe(false);
    expect(item(renderer, "closeRight")[0].props.disabled).toBe(false);
    expect(item(renderer, "closeNonAgent")[0].props.disabled).toBe(false);
    expect(item(renderer, "download")).toHaveLength(1);
    await act(async () => item(renderer, "closeRight")[0].props.onClick());
    expect(onCloseRight).toHaveBeenCalledWith(strip[2], commandScope);
    await openMenuOn(renderer, 2);
    await act(async () => item(renderer, "closeNonAgent")[0].props.onClick());
    expect(onCloseNonAgent).toHaveBeenCalledWith(commandScope);

    // The last tab, and a diff: nothing to its right, and no file behind it.
    await openMenuOn(renderer, 3);
    expect(item(renderer, "closeRight")[0].props.disabled).toBe(true);
    expect(item(renderer, "closeOthers")[0].props.disabled).toBe(false);
    expect(item(renderer, "download")).toHaveLength(0);

    // Frozen writes: a set holding tmux windows needs the same permission a
    // single terminal close does, while a set of documents does not.
    await act(async () => { renderer.update(element(false)); });
    await openMenuOn(renderer, 2);
    expect(item(renderer, "closeOthers")[0].props.disabled).toBe(true);
    expect(item(renderer, "closeRight")[0].props.disabled).toBe(false);
    expect(item(renderer, "closeNonAgent")[0].props.disabled).toBe(true);
    await act(async () => item(renderer, "download")[0].props.onClick());
    expect(onDownloadTab).toHaveBeenCalledWith(strip[2]);
    await act(async () => renderer.unmount());
  });

  it("keeps a workspace menu command bound to the connection scope that opened it", async () => {
    const onWorkspaceCommand = vi.fn();
    const replacementScope = { ...commandScope, connectionEpoch: 2, serverIdentity: "server-b" };
    const element = (scope: typeof commandScope) => <WorkspaceSidebar
      adapters={[]} agents={[]} agentSort="workspace" agentsRatio={0.4} canMutate commandScope={scope} compactWorkspaces={false}
      hostLabel="remote-linux" maxWidth={426} phase="connected" rows={rows} stateGlyphs={false} transport="ssh" width={240}
      onAgentsRatio={noop} onLaunchAgent={noop} onOpenSettings={noop} onRenameAgent={noop} onResumeAgent={noop}
      onReviewHooks={noop} onSelectAgent={noop} onSelectWorkspace={noop} onSortMode={noop} onWidth={noop}
      onWorkspaceCommand={onWorkspaceCommand}
    />;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(commandScope)); });
    await act(async () => renderer.root.findByProps({ "data-workspace-index": 0 }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    await act(async () => { renderer.update(element(replacementScope)); });
    await act(async () => renderer.root.findByProps({ "data-menu-item": "rename" }).props.onClick());
    expect(onWorkspaceCommand).toHaveBeenCalledWith(session, "session.rename", commandScope);
    await act(async () => renderer.unmount());
  });

  it("invalidates an agent menu and palette subject when the connection replaces recycled IDs", async () => {
    const onSelectAgent = vi.fn();
    const replacementScope = { ...commandScope, connectionEpoch: 2, serverIdentity: "server-b" };
    const agentRows = buildAgentRows(
      [agent({ id: "recycled", paneId: "%1", displayName: "Old agent" })],
      () => ({ workspaceOrder: 0, workspaceName: "work" }),
      () => true,
      "workspace",
    );
    const element = (scope: typeof commandScope, displayName: string) => <WorkspaceSidebar
      adapters={[]} agents={agentRows.map((row) => ({ ...row, agent: { ...row.agent, displayName } }))}
      agentSort="workspace" agentsRatio={0.4} canMutate commandScope={scope} compactWorkspaces={false} hostLabel="remote-linux" maxWidth={426}
      phase="connected" rows={[]} stateGlyphs={false} transport="ssh" width={240}
      onAgentsRatio={noop} onLaunchAgent={noop} onOpenSettings={noop} onRenameAgent={noop} onResumeAgent={noop}
      onReviewHooks={noop} onSelectAgent={onSelectAgent} onSelectWorkspace={noop} onSortMode={noop} onWidth={noop}
      onWorkspaceCommand={noop}
    />;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(commandScope, "Old agent")); });
    const oldRow = renderer.root.findByProps({ "data-agent-index": 0 });
    await act(async () => oldRow.props.onFocus());
    await act(async () => oldRow.props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    expect(renderer.root.findAllByProps({ "data-menu-item": "focus" })).toHaveLength(1);
    expect(rowCommandRegistry.available()).toContain("agents.focusRow");

    await act(async () => { renderer.update(element(replacementScope, "Replacement agent")); });
    expect(renderer.root.findAllByProps({ "data-menu-item": "focus" })).toHaveLength(0);
    expect(rowCommandRegistry.available()).toEqual([]);
    expect(onSelectAgent).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("keeps Files and Git mutually exclusive in the one right panel", () => {
    const html = renderToStaticMarkup(<RightPanel
      files={<p>files surface</p>} git={<p>git surface</p>} maxWidth={800} onSurface={noop}
      onWidth={noop} surface="git" width={320}
    />);
    expect(html).toContain('aria-selected="false"');
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain('aria-labelledby="panel-tab-git"');
    expect(html).toContain('id="panel-surface-git"');
    expect(html).toContain("git surface");
    expect(html).not.toContain("files surface");
    // Resizable from its left edge, like the sidebar is from its right.
    expect(html).toContain("--panel-width:320px");
    expect(html).toContain('aria-label="Resize the panel"');
  });

  it("puts four controls and an unread count on the titlebar, and no more", () => {
    const html = renderToStaticMarkup(<TitleBar
      canMutate onBell={noop} onNewWorkspace={noop} onTogglePanel={noop}
      onToggleSidebar={noop} panelOpen={false} platform="mac" sidebarOpen unread={3} workspaceName="muxflow"
    />);
    expect([...html.matchAll(/<button/gu)]).toHaveLength(4);
    expect(html).toContain("3 agents waiting; jump to the loudest");
    expect(html).toContain("muxflow");
    // The branch label is gone: it arrived a beat after the first paint and
    // changed the bar's content height when it did.
    expect(html).not.toContain("titlebar-branch");
    const quiet = renderToStaticMarkup(<TitleBar
      canMutate onBell={noop} onNewWorkspace={noop} onTogglePanel={noop}
      onToggleSidebar={noop} panelOpen={false} platform="linux" sidebarOpen={false} unread={0}
    />);
    expect(quiet).toContain("No agents waiting");
    expect(quiet).toContain("No workspace");
  });

  it("reserves traffic-light room on macOS only, because only macOS overlays them", () => {
    const bar = (platform: "mac" | "linux") => renderToStaticMarkup(<TitleBar
      canMutate onBell={noop} onNewWorkspace={noop} onTogglePanel={noop}
      onToggleSidebar={noop} panelOpen={false} platform={platform} sidebarOpen unread={0}
    />);
    // `titleBarStyle: "Overlay"` is a macOS-only Tauri option; a Linux window
    // keeps its native decorations, so the 78px reservation there would be
    // dead space beside a real title bar.
    expect(bar("mac")).toContain("titlebar-overlay");
    expect(bar("linux")).not.toContain("titlebar-overlay");
    // Everything else about the bar is identical across platforms.
    expect([...bar("linux").matchAll(/<button/gu)]).toHaveLength([...bar("mac").matchAll(/<button/gu)].length);
  });

  it("shows nothing while connected, and one explained line once the trouble persists", () => {
    vi.useFakeTimers();
    try {
      const strip = (phase: ConnectionPhase, detail = "") => <DisconnectedStrip
        detail={detail} hasSnapshot onOpenSettings={noop} onReconnect={noop} phase={phase}
      />;
      const rendered = (renderer: ReturnType<typeof create>) => JSON.stringify(renderer.toJSON());
      let renderer!: ReturnType<typeof create>;
      act(() => { renderer = create(strip("connected")); });
      expect(renderer.toJSON()).toBeNull();

      // A blip that heals inside the delay never becomes a banner: the native
      // link repairs a sequence break in place, and a strip that flashed for
      // 200ms was unreadable churn over the terminal.
      act(() => { renderer.update(strip("resyncing")); });
      act(() => { vi.advanceTimersByTime(STRIP_APPEAR_DELAY_MS - 1); });
      expect(renderer.toJSON()).toBeNull();
      act(() => { renderer.update(strip("connected")); });
      act(() => { vi.advanceTimersByTime(5_000); });
      expect(renderer.toJSON()).toBeNull();

      act(() => { renderer.update(strip("reconnecting", "network unreachable")); });
      act(() => { vi.advanceTimersByTime(STRIP_APPEAR_DELAY_MS); });
      const reconnecting = rendered(renderer);
      expect(reconnecting).toContain("link-strip");
      expect(reconnecting).toContain("Reconnecting to tmux…");
      expect(reconnecting).toContain("network unreachable");
      expect(reconnecting).toContain("Reconnect");
      expect(reconnecting).toContain('"role":"status"');
      // Recovery is never delayed, only the appearance is.
      act(() => { renderer.update(strip("connected")); });
      expect(renderer.toJSON()).toBeNull();

      act(() => { renderer.update(strip("readOnly")); });
      act(() => { vi.advanceTimersByTime(STRIP_APPEAR_DELAY_MS); });
      const frozen = rendered(renderer);
      // `status`, not `alert`: read-only is a persistent condition, and an
      // assertive region would re-interrupt on every detail re-render.
      expect(frozen).toContain('"role":"status"');
      expect(frozen).toContain("Connected read-only");
      expect(frozen).toContain("writes are frozen");
      // Read-only is not something "Reconnect" fixes, so it is not offered.
      expect(frozen).not.toContain("Reconnect");
      act(() => { renderer.unmount(); });
    } finally {
      vi.useRealTimers();
    }
  });

  it("overlays the shell body with the strip instead of taking a row from it", () => {
    // In flow, this band moved the terminal surface the tmux client size is
    // measured from: appearing and recovering each resized the user's real
    // windows. It is an overlay in its own containing block now.
    expect(stylesCss).toMatch(/\.link-strip \{\s*position: absolute; z-index: 30; top: 0; left: 0; right: 0;/u);
    expect(stylesCss).toMatch(/\.shell-body \{ position: relative;/u);
  });

  it("shows exact hook events, command, owner, path, and Codex trust guidance", () => {
    const html = renderToStaticMarkup(<HookReviewDialog applying={false} review={{
      adapterId: "codex", action: "install", revision: "token", alreadyInstalled: false,
      managedLabel: "v1", trustGuidance: "Review and trust this integration with /hooks; trust is not managed automatically.",
      changes: [{ path: "/home/me/.codex/hooks.json", summary: "Add hook", owner: "Codex adapter", command: "/usr/bin/muxflow-host hook ingest", events: ["Stop", "PermissionRequest"], beforeHash: "", afterHash: "sha256:new", createsConfig: true, removesConfig: false, beforePreview: "{}", afterPreview: "{\"token\":\"<redacted>\"}", diffPreview: "--- before\n-{}\n+++ after\n+{\"token\":\"<redacted>\"}", previewTruncated: true }],
    }} onCancel={noop} onConfirm={noop} />);
    expect(html).toContain("/home/me/.codex/hooks.json");
    expect(html).toContain("/usr/bin/muxflow-host hook ingest");
    expect(html).toContain("Stop, PermissionRequest");
    expect(html).toContain("Codex adapter");
    expect(html).toContain("/hooks");
    expect(html).toContain("Create config");
    expect(html).toContain("sha256:new");
    expect(html).toContain("Redacted configuration diff");
    expect(html).toContain("&lt;redacted&gt;");
    expect(html).toContain("Preview truncated");
    expect(html).toContain('class="modal-backdrop hook-review-backdrop"');
    expect(html).toContain('class="hook-review-scroll"');
    expect(html).not.toContain("trust is managed");
  });
});

describe("saved host picker", () => {
  const profiles = [
    { id: "local", label: "Local", connection: { mode: "local" } },
    { id: "ssh-remote-linux", label: "remote-linux", connection: { mode: "ssh", profileId: "ssh-remote-linux", target: "remote-linux" } },
  ] as const;

  const settings = (overrides: Partial<Parameters<typeof SettingsDialog>[0]> = {}) => <SettingsDialog
    agentSetup={{ available: false, connected: true, reports: true, onSetUp: noop }}
    connectionMode="local"
    helper={{ phase: "idle" }}
    onAddHost={noop}
    onClose={noop}
    onConnect={noop}
    onConnectionMode={noop}
    onDeleteProfile={noop}
    onProbeHelper={noop}
    onProfile={noop}
    onRequestHelperInstall={noop}
    onNotificationStatus={async () => "authorized"}
    onShell={noop}
    onSounds={noop}
    onSshConfigPath={noop}
    onTestNotification={async () => undefined}
    onSshTarget={noop}
    profiles={profiles as unknown as HostProfile[]}
    remote={false}
    selectedProfileId=""
    shell={{ agentStateGlyphs: false, terminalScreenReader: false } as ShellState}
    sounds={{ enabled: true, blocked: "subtle", completed: "subtle", volume: 0.5 }}
    sshConfigPath=""
    sshTarget=""
    {...overrides}
  />;

  it("exposes the persisted compact workspace preference in workspace settings", async () => {
    const onShell = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(settings({ onShell, shell: { ...defaultShellState, compactWorkspaces: true } })); });
    const workspaceTab = renderer.root.findAllByType("button").find((node) => node.props.children === "Workspace")!;
    await act(async () => { workspaceTab.props.onClick(); });
    const toggle = renderer.root.findAllByType("input").find((node) => node.props.checked === true)!;
    expect(toggle.props.type).toBe("checkbox");
    await act(async () => { toggle.props.onChange({ target: { checked: false } }); });
    expect(onShell).toHaveBeenCalledWith({ compactWorkspaces: false });
    await act(async () => renderer.unmount());
  });

  it("commits a typed terminal font size once, when the field is done being typed into", async () => {
    const onShell = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(settings({ onShell, shell: defaultShellState })); });
    const terminalTab = renderer.root.findAllByType("button").find((node) => node.props.children === "Terminal")!;
    await act(async () => { terminalTab.props.onClick(); });
    const field = () => renderer.root.findByProps({ "aria-label": "Terminal font size" });
    expect(field().props).toMatchObject({ min: 10, max: 20, step: "1", type: "number", value: "13" });

    // Typing "18" is two keystrokes, and the first of them is a 1. Clamping per
    // keystroke turned that into 10 and then 20 — the size the user asked for
    // was the one value the field could not produce.
    await act(async () => { field().props.onChange({ target: { value: "1" } }); });
    await act(async () => { field().props.onChange({ target: { value: "18" } }); });
    expect(onShell).not.toHaveBeenCalled();
    expect(field().props.value).toBe("18");
    await act(async () => { field().props.onBlur(); });
    expect(onShell).toHaveBeenCalledTimes(1);
    expect(onShell).toHaveBeenCalledWith({ terminalFontSize: 18 });

    // Emptying the field is a step on the way to another number, not a request
    // for a 0px terminal.
    onShell.mockClear();
    await act(async () => { field().props.onChange({ target: { value: "" } }); });
    await act(async () => { field().props.onBlur(); });
    expect(onShell).not.toHaveBeenCalled();
    expect(field().props.value).toBe("13");

    // Enter commits without waiting for the field to lose focus, and the clamp
    // still owns the range.
    const preventDefault = vi.fn();
    await act(async () => { field().props.onChange({ target: { value: "99" } }); });
    await act(async () => { field().props.onKeyDown({ key: "Enter", preventDefault }); });
    expect(preventDefault).toHaveBeenCalled();
    expect(onShell).toHaveBeenCalledWith({ terminalFontSize: 20 });
    await act(async () => renderer.unmount());
  });

  it("reports the OS notification permission and what the user would do about it", async () => {
    const soundsTab = (renderer: ReturnType<typeof create>) =>
      renderer.root.findAllByType("button").find((node) => node.props.children === "Sounds")!;
    const shown = async (status: string) => {
      let renderer!: ReturnType<typeof create>;
      await act(async () => { renderer = create(settings({ onNotificationStatus: async () => status as never })); });
      await act(async () => { soundsTab(renderer).props.onClick(); });
      const text = JSON.stringify(renderer.toJSON());
      await act(async () => renderer.unmount());
      return text;
    };
    // The three states are three different answers, and the difference is the
    // whole reason the line exists: "denied" is a setting to change, "not
    // requested" is a button to press, "unsupported" is the wrong build.
    expect(await shown("denied")).toContain("Notifications are turned off for this app.");
    expect(await shown("notDetermined")).toContain("sending a test notification is what asks for permission");
    expect(await shown("unsupported")).toContain("did not report a notification permission this app understands");
    expect(await shown("authorized")).toContain("Notifications are allowed for this app.");
  });

  it("sends a test notification and shows the exact failure beside the button", async () => {
    // The command rejects; nothing in between converts that into a resolved
    // string, so a rejection cannot arrive as a silent success.
    const onTestNotification = vi.fn(async () => {
      throw new Error("macOS notification permission is denied; enable it in System Settings");
    });
    const onNotificationStatus = vi.fn(async () => "notDetermined" as never);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(settings({ onNotificationStatus, onTestNotification })); });
    await act(async () => {
      renderer.root.findAllByType("button").find((node) => node.props.children === "Sounds")!.props.onClick();
    });
    const button = () => renderer.root.findAllByType("button")
      .find((node) => String(node.props.children).includes("Send test notification"))!;
    await act(async () => { button().props.onClick(); });
    expect(onTestNotification).toHaveBeenCalledTimes(1);
    const text = JSON.stringify(renderer.toJSON());
    // Inline and announced, not a toast: this is a diagnostic the user pressed
    // a button to get, and it belongs beside the button that produced it.
    expect(text).toContain("macOS notification permission is denied");
    expect(text).toContain('"role":"alert"');
    // The prompt this button raises can change the permission, so the line
    // above it is re-read once the attempt settles.
    expect(onNotificationStatus).toHaveBeenCalledTimes(2);
    await act(async () => renderer.unmount());
  });

  it("shows the host the user picked, not the one the app is connected to", () => {
    // The defect: the control derived its value by matching each saved profile
    // against the *live* connection, so picking a different host left the
    // picker showing the connected one until Connect was pressed.
    const onProfile = vi.fn();
    let renderer!: ReturnType<typeof create>;
    act(() => { renderer = create(settings({ onProfile })); });
    const picker = renderer.root.findByProps({ "aria-label": "Host" });
    expect(picker.props.value).toBe("");
    act(() => picker.props.onChange({ target: { value: "ssh-remote-linux" } }));
    expect(onProfile).toHaveBeenCalledWith(profiles[1]);
    act(() => { renderer.update(settings({ onProfile, selectedProfileId: "ssh-remote-linux" })); });
    expect(renderer.root.findByProps({ "aria-label": "Host" }).props.value).toBe("ssh-remote-linux");
    act(() => renderer.unmount());
  });

  it("lists saved hosts only, and says so when the form is not one of them", () => {
    // The list used to open on "Current values (not saved)" — an entry that was
    // neither a saved host nor anything the user had chosen, and the reason
    // Connect could not tell "edit this machine" from "add another one".
    const onAddHost = vi.fn();
    const onProfile = vi.fn();
    let renderer!: ReturnType<typeof create>;
    act(() => { renderer = create(settings({ onAddHost, onProfile, selectedProfileId: "ssh-remote-linux" })); });
    const options = () => renderer.root.findAllByType("option").map((node) => ({
      value: node.props.value, label: node.props.children, disabled: node.props.disabled,
    }));
    expect(JSON.stringify(renderer.toJSON())).not.toContain("Current values");
    expect(options()).toEqual([
      { value: "local", label: "Local", disabled: undefined },
      { value: "ssh-remote-linux", label: "remote-linux", disabled: undefined },
    ]);

    // Nothing saved is being edited: the picker says which state the form is
    // in rather than showing someone else's host, and the entry cannot be
    // chosen because it is not a place to go.
    act(() => { renderer.update(settings({ onAddHost, onProfile, selectedProfileId: "" })); });
    expect(options()[0]).toEqual({ value: "", label: "New host…", disabled: true });
    expect(renderer.root.findByProps({ "aria-label": "Host" }).props.value).toBe("");

    // Picking a real host is what leaves that state; the placeholder goes with
    // it once the shell has recorded the selection.
    act(() => renderer.root.findByProps({ "aria-label": "Host" })
      .props.onChange({ target: { value: "ssh-remote-linux" } }));
    expect(onProfile).toHaveBeenCalledWith(profiles[1]);
    act(() => { renderer.update(settings({ onAddHost, onProfile, selectedProfileId: "ssh-remote-linux" })); });
    expect(options().some((option) => option.label === "New host…")).toBe(false);
    act(() => renderer.unmount());
  });

  it("offers a way to a host that is not saved yet, beside the picker", () => {
    const onAddHost = vi.fn();
    let renderer!: ReturnType<typeof create>;
    act(() => { renderer = create(settings({ onAddHost, selectedProfileId: "ssh-remote-linux" })); });
    const add = renderer.root.findAllByType("button").find((node) => String(node.children).includes("Add host"))!;
    act(() => add.props.onClick());
    expect(onAddHost).toHaveBeenCalledTimes(1);
    act(() => renderer.unmount());
  });

  it("offers Delete only for a picked host, names it, and says why when it cannot", () => {
    const onDeleteProfile = vi.fn();
    const deleteButton = (renderer: ReturnType<typeof create>) =>
      renderer.root.findAllByType("button").find((node) => String(node.children).includes("Delete"))!;
    let renderer!: ReturnType<typeof create>;
    act(() => { renderer = create(settings({ onDeleteProfile })); });
    expect(deleteButton(renderer).props.disabled).toBe(true);
    // A disabled control that does not say why reads as broken.
    expect(JSON.stringify(renderer.toJSON())).toContain("Pick a saved host above to remove it.");

    act(() => { renderer.update(settings({ onDeleteProfile, deletableProfile: profiles[1] as unknown as HostProfile, selectedProfileId: "ssh-remote-linux" })); });
    const enabled = deleteButton(renderer);
    expect(enabled.props.disabled).toBe(false);
    // A verb with an object: "Delete host…" beside a combobox names nothing.
    expect(String(enabled.children)).toContain("remote-linux");
    act(() => enabled.props.onClick());
    expect(onDeleteProfile).toHaveBeenCalled();
    act(() => renderer.unmount());
  });

  it("explains the one refusal the store makes rather than leaving it a mystery", () => {
    let renderer!: ReturnType<typeof create>;
    act(() => { renderer = create(settings({ profiles: [profiles[0]] as unknown as HostProfile[], selectedProfileId: "local" })); });
    expect(JSON.stringify(renderer.toJSON())).toContain("The last saved host cannot be removed.");
    act(() => renderer.unmount());
  });
});

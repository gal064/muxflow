// @vitest-environment jsdom
import stylesCss from "../../styles.css?raw";
import { renderToStaticMarkup } from "react-dom/server";
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { HostProfile, Session } from "../../app/types";
import { rowCommandRegistry } from "../../commands/rowCommands";
import { agent } from "../agents/testFixtures";
import type { AgentAdapterDescriptor } from "../agents/types";
import { buildAgentRows } from "../agents/agentsList";
import { HookReviewDialog } from "../agents/HookReviewDialog";
import { WorkspaceSidebar } from "../workspaces/WorkspaceSidebar";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../workspaces/TabStrip";
import { workspaceRows, type WorkspaceRowModel } from "../workspaces/workspaceRows";
import { deriveAgentRollups } from "../agents/selectors";
import { DisconnectedStrip, STRIP_APPEAR_DELAY_MS } from "./DisconnectedStrip";
import type { ConnectionPhase } from "../../state/connectionReducer";
import { RightPanel } from "./RightPanel";
import type { ShellState } from "./types";
import { SettingsDialog } from "./SettingsDialog";
import { TitleBar } from "./TitleBar";

const noop = vi.fn();
const commandScope = { hostProfileId: "remote", connectionKey: "ssh:remote", connectionEpoch: 1, serverIdentity: "server-a", generation: 1 };
const session: Session = { id: "$1", name: "A very long workspace name", windowCount: 3, attachedClients: 1, order: 0 };
const rows: WorkspaceRowModel[] = [{
  session, active: true, attention: "blocked", unread: 2, working: true,
  agents: [{ id: "a1", name: "codex", state: "blocked" }], agentOverflow: 0,
  branch: "main*", path: "~/dev/muxflow",
}];

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

const sidebar = (overrides: Partial<Parameters<typeof WorkspaceSidebar>[0]> = {}) => renderToStaticMarkup(<WorkspaceSidebar
  adapters={[]}
  agents={buildAgentRows([agent({ displayName: "Codex one", lifecycle: "blocked" })], () => ({ workspaceOrder: 0, workspaceName: "work", tabIndex: 1 }), () => true, "workspace")}
  agentSort="workspace"
  agentsRatio={0.4}
  canMutate
  commandScope={commandScope}
  hostLabel="remote-linux"
  latencyMs={41}
  onAgentsRatio={noop}
  onLaunchAgent={noop}
  onOpenSettings={noop}
  onRenameAgent={noop}
  onResumeAgent={noop}
  onReviewHooks={noop}
  onSelectAgent={noop}
  onSelectWorkspace={noop}
  onSortMode={noop}
  onWorkspaceCommand={noop}
  phase="connected"
  rows={rows}
  maxWidth={426}
  onWidth={noop}
  stateGlyphs={false}
  transport="ssh"
  width={240}
  {...overrides}
/>);

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
    expect(html).toContain("Agent ordering: workspace. Switch to status.");
    // The only resting connection indicator, and it is the way into settings.
    expect(html).toContain("Host remote-linux over ssh, connected. Open connection settings.");
    expect(html).toContain("41 ms");
  });

  it("badges only the workspaces and agents that are waiting on a human", () => {
    // The badge itself is decorative, so the count has to be in the row's own
    // accessible name or a screen reader never hears it.
    expect(sidebar()).toContain('aria-label="A very long workspace name, codex · blocked, 2 agents waiting"');
    expect(sidebar()).toContain("Codex one, blocked, waiting, work, tab 1");
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
    expect(busy).toContain("claude · done, unread");
    expect(busy).toContain("aider · working");
    expect(busy).toContain("…2 more");
    // Four lines, one announcement: the label names the loudest and counts the
    // rest rather than reading every line of one list item.
    expect(busy).toContain('aria-label="A very long workspace name, codex · blocked, 5 agents, 2 agents waiting"');
    // Nothing to count means nothing is said about counting.
    expect(sidebar()).not.toContain("more");
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
      adapters={adapters} agents={rows} agentSort="workspace" agentsRatio={0.4} canMutate={canMutate} commandScope={commandScope}
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

  it("encodes every agent state in the dot's class, which is where the color comes from", () => {
    // The dot has no background of its own: `.state-dot.working` and friends
    // carry it. A dot rendered as bare `state-dot` is an invisible 8x8 box, and
    // the agents section loses the whole encoding the mock is built around —
    // which is exactly what shipped when this class stopped interpolating.
    for (const [lifecycle, expected] of [["working", "working"], ["blocked", "blocked"], ["idle", "idle"]] as const) {
      const html = sidebar({
        agents: buildAgentRows([agent({ displayName: "A", lifecycle })], () => ({ workspaceOrder: 0, workspaceName: "work" }), () => true, "workspace"),
      });
      expect(html, lifecycle).toContain(`class="state-dot ${expected}"`);
    }
    // Every state the list can produce must have a rule to match, or the same
    // defect returns for one state instead of all of them.
    // Read the same way `theme.test.ts` reads `tokens.css`: the real file.
    for (const state of ["working", "blocked", "done", "unknown", "idle"]) {
      expect(stylesCss, state).toContain(`.state-dot.${state}`);
    }
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

  it("renders a working spinner and distinct blocked, unread-complete, and idle tab marks", () => {
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
    expect(html).toContain('class="tab-dot idle"');
    expect(stylesCss).toContain(".tab-dot.done { background: var(--ok); }");
    expect(stylesCss).toContain("@media (prefers-reduced-motion: no-preference)");
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
      adapters={[]} agents={[]} agentSort="workspace" agentsRatio={0.4} canMutate commandScope={scope}
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
      agentSort="workspace" agentsRatio={0.4} canMutate commandScope={scope} hostLabel="remote-linux" maxWidth={426}
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

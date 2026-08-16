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
import { DisconnectedStrip } from "./DisconnectedStrip";
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
  hostLabel="omarchy"
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
    // The branch, and only the branch. The working directory used to be fused
    // onto this line and is now ⌘P's business alone.
    expect(html).toContain('class="workspace-meta">main*<');
    expect(html).not.toContain("~/dev/muxflow");
    // The one control in the agents header names both its state and its effect.
    expect(html).toContain("Agent ordering: workspace. Switch to status.");
    // The only resting connection indicator, and it is the way into settings.
    expect(html).toContain("Host omarchy over ssh, connected. Open connection settings.");
    expect(html).toContain("41 ms");
  });

  it("badges only the workspaces and agents that are waiting on a human", () => {
    // The badge itself is decorative, so the count has to be in the row's own
    // accessible name or a screen reader never hears it.
    expect(sidebar()).toContain('aria-label="A very long workspace name, codex · blocked, 2 agents waiting, main*"');
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
    expect(busy).toContain('aria-label="A very long workspace name, codex · blocked, 5 agents, 2 agents waiting, main*"');
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
      supportsProcessDetection: true, supportsScreenFallback: false, hookConfigPath: "~/.codex/config.toml",
      hookEvents: [], placements: ["window", "split"], hookWiring: "wired", hookWiringDetail: "", hookSetupRecommended: false,
    }];
    let renderer!: ReturnType<typeof create>;
    const element = (rows: typeof agents, canMutate = true) => <WorkspaceSidebar
      adapters={adapters} agents={rows} agentSort="workspace" agentsRatio={0.4} canMutate={canMutate} commandScope={commandScope}
      hostLabel="omarchy" latencyMs={41} maxWidth={426} phase="connected" rows={[]} stateGlyphs={false} transport="ssh" width={240}
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
      activeKey="app:file" canMutate canSplit commandScope={commandScope} stateGlyphs={false} onClose={noop} onMove={noop}
      onNewTerminal={noop} onPin={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={[
        { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "blocked" },
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

  it("keeps a tab menu command bound to the connection scope that opened it", async () => {
    const onClose = vi.fn();
    const tab = { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "none" } as const;
    const replacementScope = { ...commandScope, connectionEpoch: 2, serverIdentity: "server-b" };
    const element = (scope: typeof commandScope) => <TabStrip
      activeKey={tab.key} canMutate canSplit commandScope={scope} stateGlyphs={false} onClose={onClose} onMove={noop}
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

  it("keeps a workspace menu command bound to the connection scope that opened it", async () => {
    const onWorkspaceCommand = vi.fn();
    const replacementScope = { ...commandScope, connectionEpoch: 2, serverIdentity: "server-b" };
    const element = (scope: typeof commandScope) => <WorkspaceSidebar
      adapters={[]} agents={[]} agentSort="workspace" agentsRatio={0.4} canMutate commandScope={scope}
      hostLabel="omarchy" maxWidth={426} phase="connected" rows={rows} stateGlyphs={false} transport="ssh" width={240}
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
      agentSort="workspace" agentsRatio={0.4} canMutate commandScope={scope} hostLabel="omarchy" maxWidth={426}
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
      files={<p>files surface</p>} git={<p>git surface</p>} onSurface={noop} surface="git"
    />);
    expect(html).toContain('aria-selected="false"');
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain('aria-labelledby="panel-tab-git"');
    expect(html).toContain('id="panel-surface-git"');
    expect(html).toContain("git surface");
    expect(html).not.toContain("files surface");
  });

  it("puts four controls and an unread count on the titlebar, and no more", () => {
    const html = renderToStaticMarkup(<TitleBar
      branch="main*" canMutate onBell={noop} onNewWorkspace={noop} onTogglePanel={noop}
      onToggleSidebar={noop} panelOpen={false} platform="mac" sidebarOpen unread={3} workspaceName="muxflow"
    />);
    expect([...html.matchAll(/<button/gu)]).toHaveLength(4);
    expect(html).toContain("3 agents waiting; jump to the loudest");
    expect(html).toContain("muxflow");
    expect(html).toContain("main*");
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

  it("shows nothing while connected, and one explained line while not", () => {
    expect(renderToStaticMarkup(<DisconnectedStrip
      detail="" hasSnapshot onOpenSettings={noop} onReconnect={noop} phase="connected"
    />)).toBe("");
    const reconnecting = renderToStaticMarkup(<DisconnectedStrip
      detail="network unreachable" hasSnapshot onOpenSettings={noop} onReconnect={noop} phase="reconnecting"
    />);
    expect(reconnecting).toContain("Reconnecting to tmux…");
    expect(reconnecting).toContain("network unreachable");
    expect(reconnecting).toContain("Reconnect");
    expect(reconnecting).toContain('role="status"');
    const frozen = renderToStaticMarkup(<DisconnectedStrip
      detail="" hasSnapshot onOpenSettings={noop} onReconnect={noop} phase="readOnly"
    />);
    // `status`, not `alert`: read-only is a persistent condition, and an
    // assertive region would re-interrupt on every detail re-render.
    expect(frozen).toContain('role="status"');
    expect(frozen).toContain("Connected read-only");
    expect(frozen).toContain("writes are frozen");
    // Read-only is not something "Reconnect" fixes, so it is not offered.
    expect(frozen).not.toContain(">Reconnect<");
  });

  it("shows exact hook events, command, owner, path, and Codex trust guidance", () => {
    const html = renderToStaticMarkup(<HookReviewDialog applying={false} review={{
      adapterId: "codex", action: "install", revision: "token", alreadyInstalled: false,
      managedLabel: "v1", trustGuidance: "Review and trust this integration with /hooks; trust is not managed automatically.",
      changes: [{ path: "/home/me/.codex/hooks.json", summary: "Add hook", owner: "Codex adapter", command: "/usr/bin/tmux-ide-host hook ingest", events: ["Stop", "PermissionRequest"], beforeHash: "", afterHash: "sha256:new", createsConfig: true, removesConfig: false, beforePreview: "{}", afterPreview: "{\"token\":\"<redacted>\"}", diffPreview: "--- before\n-{}\n+++ after\n+{\"token\":\"<redacted>\"}", previewTruncated: true }],
    }} onCancel={noop} onConfirm={noop} />);
    expect(html).toContain("/home/me/.codex/hooks.json");
    expect(html).toContain("/usr/bin/tmux-ide-host hook ingest");
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
    { id: "ssh-omarchy", label: "omarchy", connection: { mode: "ssh", profileId: "ssh-omarchy", target: "omarchy" } },
  ] as const;

  const settings = (overrides: Partial<Parameters<typeof SettingsDialog>[0]> = {}) => <SettingsDialog
    agentSetup={{ available: false, connected: true, reports: true, onSetUp: noop }}
    connectionMode="local"
    helper={{ phase: "idle" }}
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
    const picker = renderer.root.findByProps({ "aria-label": "Saved host" });
    expect(picker.props.value).toBe("");
    act(() => picker.props.onChange({ target: { value: "ssh-omarchy" } }));
    expect(onProfile).toHaveBeenCalledWith(profiles[1]);
    act(() => { renderer.update(settings({ onProfile, selectedProfileId: "ssh-omarchy" })); });
    expect(renderer.root.findByProps({ "aria-label": "Saved host" }).props.value).toBe("ssh-omarchy");
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

    act(() => { renderer.update(settings({ onDeleteProfile, deletableProfile: profiles[1] as unknown as HostProfile, selectedProfileId: "ssh-omarchy" })); });
    const enabled = deleteButton(renderer);
    expect(enabled.props.disabled).toBe(false);
    // A verb with an object: "Delete host…" beside a combobox names nothing.
    expect(String(enabled.children)).toContain("omarchy");
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

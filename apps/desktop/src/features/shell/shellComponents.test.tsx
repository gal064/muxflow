import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { Session } from "../../app/types";
import { agent } from "../agents/testFixtures";
import { buildAgentRows } from "../agents/agentsList";
import { HookReviewDialog } from "../agents/HookReviewDialog";
import { WorkspaceSidebar } from "../workspaces/WorkspaceSidebar";
import { TabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../workspaces/TabStrip";
import type { WorkspaceRowModel } from "../workspaces/workspaceRows";
import { DisconnectedStrip } from "./DisconnectedStrip";
import { RightPanel } from "./RightPanel";
import { TitleBar } from "./TitleBar";

const noop = vi.fn();
const session: Session = { id: "$1", name: "A very long workspace name", windowCount: 3, attachedClients: 1, order: 0 };
const rows: WorkspaceRowModel[] = [{
  session, active: true, attention: "blocked", unread: 2, working: true,
  activity: "codex · blocked", metadata: "main* · ~/dev/muxflow",
}];

const sidebar = (overrides: Partial<Parameters<typeof WorkspaceSidebar>[0]> = {}) => renderToStaticMarkup(<WorkspaceSidebar
  adapters={[]}
  agents={buildAgentRows([agent({ displayName: "Codex one", lifecycle: "blocked" })], () => ({ workspaceOrder: 0, workspaceName: "work", tabIndex: 1 }), () => true, "grouped")}
  agentSort="grouped"
  agentsRatio={0.4}
  canMutate
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
    expect(html).toContain("main* · ~/dev/muxflow");
    // The one control in the agents header names both its state and its effect.
    expect(html).toContain("Agent ordering: grouped. Switch to priority.");
    // The only resting connection indicator, and it is the way into settings.
    expect(html).toContain("Host omarchy over ssh, connected. Open connection settings.");
    expect(html).toContain("41 ms");
  });

  it("badges only the workspaces and agents that are waiting on a human", () => {
    // The badge itself is decorative, so the count has to be in the row's own
    // accessible name or a screen reader never hears it.
    expect(sidebar()).toContain('aria-label="A very long workspace name, codex · blocked, 2 agents waiting, main* · ~/dev/muxflow"');
    expect(sidebar()).toContain("Codex one, blocked, waiting, work, tab 1");
    const quiet = sidebar({
      agents: buildAgentRows([agent({ displayName: "Claude", lifecycle: "working" })], () => ({ workspaceOrder: 0, workspaceName: "work" }), () => true, "grouped"),
      rows: [{ ...rows[0], unread: 0, attention: "working", working: true, activity: undefined }],
    });
    expect(quiet).not.toContain("waiting");
    expect(quiet).not.toContain('class="badge badge-row"');
  });

  it("states empty sidebar sections in one line each", () => {
    const html = sidebar({ rows: [], agents: [] });
    expect(html).toContain("No tmux sessions on this host yet.");
    expect(html).toContain("No agents detected.");
  });

  it("renders combined terminal/app tabs as one selected tablist", () => {
    const html = renderToStaticMarkup(<TabStrip
      activeKey="app:file" canMutate canSplit onClose={noop} onMove={noop}
      onNewTerminal={noop} onRenameTerminal={noop} onSelect={noop} onSplit={noop}
      tabs={[
        { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 1, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "blocked" },
        { key: "app:file", kind: "app", id: "file", title: "README.md", appKind: "markdown", resource: "/r/README.md", order: 0, canMoveLeft: false, canMoveRight: false },
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
    expect(frozen).toContain('role="alert"');
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

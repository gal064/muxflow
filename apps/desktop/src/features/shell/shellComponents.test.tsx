import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { Session } from "../../app/types";
import { AgentSidebar } from "../agents/AgentSidebar";
import { agent } from "../agents/testFixtures";
import { HookReviewDialog } from "../agents/HookReviewDialog";
import { WorkspaceRail } from "../workspaces/WorkspaceRail";
import { CombinedTabStrip, workspaceTabDomId, workspaceTabPanelDomId } from "../workspaces/CombinedTabStrip";
import { ConnectionBanner } from "./ConnectionBanner";
import { ExplorerGitSidebar } from "./ExplorerGitSidebar";

const noop = vi.fn();
const sessions: Session[] = [{ id: "$1", name: "A very long workspace name", windowCount: 3, attachedClients: 1, order: 0 }];

describe("application shell accessibility contracts", () => {
  it("renders a named, selected, full-name workspace rail with mutation routes", () => {
    const html = renderToStaticMarkup(<WorkspaceRail activeSessionId="$1" canMutate sessions={sessions}
      onCommand={noop} onCreate={noop} onSelect={noop} />);
    expect(html).toContain('aria-label="tmux workspaces"');
    expect(html).toContain('aria-current="page"');
    expect(html).toContain("A very long workspace name");
    expect(html).toContain("Move up");
    expect(html).toContain("Close…");
  });

  it("renders combined terminal/app tabs as one selected tablist", () => {
    const html = renderToStaticMarkup(<CombinedTabStrip
      activeKey="app:file" canMutate commandMenu={null} onClose={noop} onMove={noop}
      onNewTerminal={noop} onOpenPalette={noop} onRenameTerminal={noop} onSelect={noop}
      tabs={[
        { key: "terminal:@1", kind: "terminal", id: "@1", title: "shell", index: 0, activeInTmux: true, zoomed: false, canMoveLeft: false, canMoveRight: false, attention: "none" },
        { key: "app:file", kind: "app", id: "file", title: "README.md", appKind: "markdown", resource: "/r/README.md", order: 0, canMoveLeft: false, canMoveRight: false },
      ]}
    />);
    expect(html).toContain('role="tablist"');
    expect(html).toContain('aria-selected="true"');
    expect(html).toContain(`id="${workspaceTabDomId("app:file")}"`);
    expect(html).toContain(`aria-controls="${workspaceTabPanelDomId("app:file")}"`);
    expect(html).toContain("README.md");
    expect(html).toContain("Close README.md");
  });

  it("keeps Explorer/Git mutually exclusive and the agent shell explicit when empty", () => {
    const sidebar = renderToStaticMarkup(<ExplorerGitSidebar
      activePane={undefined} collapsed={false} connection={{ mode: "local" }} connectionMode="local"
      onConnect={noop} onConnectionMode={noop} onProfile={noop} onSshConfigPath={noop}
      onSshTarget={noop} onSurface={noop} onToggleCollapsed={noop} profiles={[]}
      sshConfigPath="" sshTarget="" surface="git"
    />);
    expect(sidebar).toContain('aria-selected="false"');
    expect(sidebar).toContain('aria-selected="true"');
    expect(sidebar).toContain('aria-labelledby="workspace-sidebar-tab-git"');
    expect(sidebar).toContain('id="workspace-sidebar-panel-git"');
    expect(sidebar).toContain('tabindex="-1"');
    expect(sidebar).toContain("Git changes for the active pane will appear here.");
    const agents = renderToStaticMarkup(<AgentSidebar agents={[]} collapsed={false} onSelect={noop} onToggle={noop} />);
    expect(agents).toContain("No agents detected");
    expect(agents).toContain("Supported agent sessions will appear here when detected.");
  });

  it("renders semantic agent attention, location, launch, hooks, and rename affordances", () => {
    const adapters = [{ id: "future-agent", displayName: "Future Agent", supportsLaunch: true, supportsResume: true, supportsHooks: true, supportsProcessDetection: true, supportsScreenFallback: false, hookConfigPath: "/future/hooks", hookEvents: ["Stop", "Blocked"], placements: ["window", "split"] as ("window" | "split")[] }];
    const html = renderToStaticMarkup(<AgentSidebar adapters={adapters} agents={[agent({ adapterId: "future-agent", lifecycle: "idle", attentionGeneration: 4, attentionKind: "completed", seenGeneration: 2 })]} collapsed={false}
      onLaunch={noop} onRename={noop} onResume={noop} onReviewHooks={noop} onSelect={noop} onToggle={noop} />);
    expect(html).toContain("Codex one, done, work, agent");
    expect(html).toContain("Unseen completion");
    expect(html).toContain("Rename agent Codex one");
    expect(html).toContain("Future Agent · new window");
    expect(html).toContain("Review Future Agent hooks");
    expect(html).toContain("Resume in new split");
    expect(html).toContain("process detection, 2 hook events");
  });

  it("keeps unmapped agents visible and non-routable", () => {
    const html = renderToStaticMarkup(<AgentSidebar agents={[agent({ sessionId: "", windowId: "", paneId: "" })]} collapsed={false} onSelect={noop} onToggle={noop} />);
    expect(html).toContain("unmapped · navigation unavailable");
    expect(html).toContain("disabled");
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

  it("explains stale write-frozen state and exposes explicit recovery", () => {
    const disconnected = renderToStaticMarkup(<ConnectionBanner
      detail="network unreachable" hasSnapshot helper={{ phase: "idle" }} onProbeHelper={noop}
      onReconnect={noop} onRequestHelperInstall={noop} phase="reconnecting" remote
    />);
    expect(disconnected).toContain("Writes are frozen and will not be queued");
    expect(disconnected).toContain("Reconnect now");
    expect(disconnected).toContain('role="status"');
    expect(disconnected).toContain("Check helper");
    const missing = renderToStaticMarkup(<ConnectionBanner detail="missing" hasSnapshot={false}
      helper={{ phase: "ready", connectionKey: "host", probe: { operatingSystem: "linux", architecture: "x86_64", tmuxVersion: "3.3a", gitVersion: "2", installed: false, compatible: false, remotePath: "/home/me/.local/bin/tmux-ide-host" } }}
      onProbeHelper={noop} onReconnect={noop} onRequestHelperInstall={noop} phase="disconnected" remote />);
    expect(missing).toContain("Install helper…");
    const live = renderToStaticMarkup(<ConnectionBanner detail="Action failed visibly" hasSnapshot
      helper={{ phase: "idle" }} onProbeHelper={noop} onReconnect={noop} onRequestHelperInstall={noop} phase="connected" remote={false} />);
    expect(live).toContain("Action failed visibly");
  });
});

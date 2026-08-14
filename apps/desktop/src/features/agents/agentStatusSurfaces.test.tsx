// @vitest-environment jsdom
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import stylesCss from "../../styles.css?raw";
import type { Session, TmuxSnapshot } from "../../app/types";
import { combineWorkspaceTabs } from "../shell/model";
import { TabStrip } from "../workspaces/TabStrip";
import { WorkspaceSidebar } from "../workspaces/WorkspaceSidebar";
import { WorkspaceSwitcher } from "../workspaces/WorkspaceSwitcher";
import { workspaceRows } from "../workspaces/workspaceRows";
import { buildAgentRows, unreadCount, jumpTarget } from "./agentsList";
import { deriveAgentRollups } from "./selectors";
import { agent } from "./testFixtures";
import type { AgentRecord } from "./types";

const noop = vi.fn();
const session: Session = { id: "$1", name: "work", windowCount: 1, attachedClients: 1, order: 0 };

const rowsFor = (agents: readonly AgentRecord[]) => buildAgentRows(
  agents,
  (record) => ({ workspaceOrder: 0, workspaceName: record.sessionName, tabIndex: 1 }),
  () => true,
  "priority",
);

const sidebar = (overrides: Partial<Parameters<typeof WorkspaceSidebar>[0]> = {}) => renderToStaticMarkup(<WorkspaceSidebar
  adapters={[]} agents={[]} agentSort="priority" agentsRatio={0.4} canMutate hostLabel="omarchy"
  latencyMs={41} maxWidth={426} onAgentsRatio={noop} onLaunchAgent={noop} onOpenSettings={noop}
  onRenameAgent={noop} onResumeAgent={noop} onReviewHooks={noop} onSelectAgent={noop}
  onSelectWorkspace={noop} onSortMode={noop} onWidth={noop} onWorkspaceCommand={noop}
  phase="connected" rows={[]} stateGlyphs={false} transport="ssh" width={240}
  {...overrides}
/>);

describe("the honest empty state", () => {
  it("says this host cannot report status, and offers to fix it", () => {
    const html = sidebar({
      hookNotice: "Agent status unavailable on this host — set up hooks",
      onSetUpHost: noop,
    });
    expect(html).toContain("Agent status unavailable on this host — set up hooks");
    expect(html).toContain('class="agents-notice"');
    expect(html).toContain("<button");
  });

  it("still lists the agents that genuinely exist, marked as unknown", () => {
    // A process detection proves an agent is there. Hiding it to avoid saying
    // something about its state would be a second dishonesty, so the row stays
    // and carries the neutral treatment instead.
    const html = sidebar({
      agents: rowsFor([agent({ lifecycle: "unknown", detectedManually: true, displayName: "inductive" })]),
      hookNotice: "Agent status unavailable on this host — set up hooks",
      onSetUpHost: noop,
    });
    expect(html).toContain("inductive");
    expect(html).toContain("state-dot unknown");
    expect(html).not.toContain("state-dot working");
  });

  it("keeps a notice that cannot be acted on out of the tab order", () => {
    const html = sidebar({ hookNotice: "Agent status unavailable on this host — its agent configuration could not be read" });
    expect(html).toContain('role="note"');
    expect(html).not.toContain("<button class=\"agents-notice\"");
  });

  it("draws an unknown dot as an outline in every place a dot appears", () => {
    // The regression this guards: unknown was a filled dot, so a detected but
    // silent agent was in the same visual class as a working one.
    for (const selector of [".state-dot.unknown", ".tab-dot.unknown"]) {
      const rule = stylesCss.slice(stylesCss.indexOf(selector));
      const block = rule.slice(0, rule.indexOf("}"));
      expect(block).toContain("background: transparent");
      expect(block).toContain("dashed");
    }
  });
});

describe("one derivation, three surfaces", () => {
  const blocked = agent({ id: "blocked", lifecycle: "blocked", attentionGeneration: 4, seenGeneration: 0 });
  const done = agent({ id: "done", lifecycle: "idle", attentionKind: "completed", attentionGeneration: 2, seenGeneration: 1, paneId: "%2", windowId: "@2" });
  const unknown = agent({ id: "unknown", lifecycle: "unknown", paneId: "%3", windowId: "@3", detectedManually: true });
  const agents = [blocked, done, unknown];
  const rollups = deriveAgentRollups(agents);

  it("gives the sidebar, the workspace row and the tab the same states", () => {
    const rows = rowsFor(agents);
    expect(rows.map((row) => row.state)).toEqual(["blocked", "done", "unknown"]);
    expect(unreadCount(rows)).toBe(2);
    expect(jumpTarget(rows)?.agent.id).toBe("blocked");

    const snapshot: TmuxSnapshot = {
      sessions: [session],
      windows: [
        { id: "@1", sessionId: "$1", index: 0, name: "claude", active: true, layout: "", zoomed: false },
        { id: "@2", sessionId: "$1", index: 1, name: "claude", active: false, layout: "", zoomed: false },
        { id: "@3", sessionId: "$1", index: 2, name: "claude", active: false, layout: "", zoomed: false },
      ],
      panes: [],
      generation: 1,
      serverIdentity: "server-a",
    } as unknown as TmuxSnapshot;
    const workspace = workspaceRows({
      snapshot, activeSessionId: "$1", agents, attentionByWorkspace: rollups.byWorkspace,
    });
    expect(workspace[0].attention).toBe("blocked");
    expect(workspace[0].unread).toBe(2);
    expect(workspace[0].working).toBe(false);

    // Three identically named windows, and each tab gets its own agent's
    // state — the aggregation is by window ID, never by name.
    const tabs = combineWorkspaceTabs(snapshot.windows, [], rollups.byWindow);
    expect(tabs.map((tab) => tab.kind === "terminal" && tab.attention))
      .toEqual(["blocked", "done", "unknown"]);
  });

  it("draws the shape option in every dot site, not only the sidebar's", () => {
    // A Phase 11 fix regressed to one of three sites. Each of the three is
    // rendered here with the option on, and each must carry the glyph class.
    const rows = rowsFor(agents);
    const surfaces = [
      sidebar({ agents: rows, stateGlyphs: true }),
      renderToStaticMarkup(<TabStrip
        activeKey="terminal:@1" canMutate canSplit onClose={noop} onMove={noop} onNewTerminal={noop}
        onRenameTerminal={noop} onSelect={noop} onSplit={noop} stateGlyphs
        tabs={combineWorkspaceTabs(
          [{ id: "@1", sessionId: "$1", index: 0, name: "claude", active: true, layout: "", zoomed: false }] as never,
          [],
          rollups.byWindow,
        )}
      />),
      renderToStaticMarkup(<WorkspaceSwitcher
        onClose={noop} onSelect={noop} stateGlyphs
        rows={[{ session, active: true, attention: "blocked", unread: 1, working: false }]}
      />),
    ];
    for (const html of surfaces) expect(html).toMatch(/class="(state|tab)-dot [a-z]+ glyphs"/);
  });
});

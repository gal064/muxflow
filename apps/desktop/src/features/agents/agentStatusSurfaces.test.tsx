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
const commandScope = { hostProfileId: "remote", connectionKey: "ssh:remote", connectionEpoch: 1, serverIdentity: "server-a", generation: 1 };
const session: Session = { id: "$1", name: "work", windowCount: 1, attachedClients: 1, order: 0 };

const rowsFor = (agents: readonly AgentRecord[]) => buildAgentRows(
  agents,
  (record) => ({ workspaceOrder: 0, workspaceName: record.sessionName, tabIndex: 1 }),
  () => true,
  "status",
);

const sidebar = (overrides: Partial<Parameters<typeof WorkspaceSidebar>[0]> = {}) => renderToStaticMarkup(<WorkspaceSidebar
  adapters={[]} agents={[]} agentSort="status" agentsRatio={0.4} canMutate commandScope={commandScope} compactWorkspaces={false} hostLabel="remote-linux"
  latencyMs={41} maxWidth={426} onAgentsRatio={noop} onLaunchAgent={noop} onOpenSettings={noop}
  onRenameAgent={noop} onResumeAgent={noop} onReviewHooks={noop} onSelectAgent={noop}
  onSelectWorkspace={noop} onTogglePinnedWorkspace={noop} onSortMode={noop} onWidth={noop} onWorkspaceCommand={noop} archivedWorkspaces={[]} onUnarchiveWorkspace={noop}
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
    expect(html).toContain(String.raw`class="agents-notice"`);
    // Text, not a control: the shell's resting-control budget is eight and is
    // spent, and this line is on screen for as long as the host is unwired.
    // The action is on the section's context menu and in Settings.
    expect(html).toContain(String.raw`role="note"`);
    expect(html).not.toContain(String.raw`<button class="agents-notice"`);
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
    // The neutral treatment rides on the adapter mark now, but it is the same
    // dashed outline the dot drew and it is still not the working one.
    expect(html).toContain("agent-mark-badge unknown");
    expect(html).not.toContain("agent-mark-badge working");
    // Unknown shares the Idle heading rather than claiming a group of its own:
    // a gap in reporting is not a fifth thing an agent can be doing.
    expect(html).toContain("<span>Idle</span>");
  });

  it("draws an unknown dot as an outline, and never as a visible glyph by default", () => {
    // Two regressions, both cascade accidents, both read from the source.
    //
    // Unknown used to be a *filled* dot, which put a detected-but-silent agent
    // in the same visual class as a working one. And when it was first made
    // hollow it also set `color`, which — at the same specificity as the
    // `:not(.glyphs)` rule that hides the glyph, but later in the file —
    // painted a literal "?" inside every 8px dot with the accessibility option
    // off. `.idle` gets away with the same shape only because its glyph is the
    // empty string.
    //
    // Asserted against the stylesheet rather than through `getComputedStyle`
    // because jsdom answers neither question: it does not resolve `var()`, so
    // every token-coloured dot computes as transparent, and it does not rank
    // two equal-specificity `color` rules — the precise thing that broke. What
    // is asserted instead is exact: the unknown rule sets no `color` at all,
    // and the glyph colour is scoped to `.glyphs`.
    for (const dot of ["state-dot", "tab-dot"]) {
      const block = stylesCss.slice(stylesCss.indexOf(`.${dot}.unknown {`));
      const body = block.slice(0, block.indexOf("}"));
      expect(body).toContain("background: transparent");
      expect(body).toContain("dashed");
      expect(body).not.toContain("color:");
      expect(stylesCss).toContain(`.${dot}.unknown.glyphs { color:`);
    }
    // And a working dot stays filled, so the two remain distinguishable.
    const working = stylesCss.slice(stylesCss.indexOf(".state-dot.working {"));
    expect(working.slice(0, working.indexOf("}"))).toContain("background: var(--state-working)");
  });

  it("gives every dialog a card class this stylesheet actually defines", () => {
    // There is no `.modal` rule in this stylesheet — each dialog names its own
    // class — so a dialog that claimed only `.modal` rendered as unstyled
    // full-bleed text across the window with its buttons in the corner. That
    // was the setup prompt, and it took a screenshot of the real app to see it.
    expect(stylesCss).not.toContain("\n.modal {");
    for (const card of ["host-setup", "hook-review", "settings-dialog", "palette"]) {
      expect(stylesCss).toContain(`.${card} {`);
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
        activeKey="terminal:@1" activeTerminalPaneCount={1} canMutate commandScope={commandScope} onClose={noop} onCloseCurrent={noop} onCloseOthers={noop}
        onCloseNonAgent={noop}
        onCloseRight={noop} onDownloadTab={noop} onMove={noop} onNewTerminal={noop}
        onPin={noop} onRenameTerminal={noop} onSelect={noop} onTogglePinned={noop} platform="mac" shortcuts={{}} stateGlyphs
        tabs={combineWorkspaceTabs(
          [{ id: "@1", sessionId: "$1", index: 0, name: "claude", active: true, layout: "", zoomed: false }] as never,
          [],
          rollups.byWindow,
        )}
      />),
      renderToStaticMarkup(<WorkspaceSwitcher
        onClose={noop} onSelect={noop} stateGlyphs
        rows={[{ session, active: true, attention: "blocked", unread: 1, working: false, pinned: false, agents: [], agentOverflow: 0 }]}
      />),
    ];
    for (const html of surfaces) expect(html).toMatch(/class="(state|tab)-dot [a-z]+ glyphs"/);
  });

  it("spins for working in ⌘P too, rather than drawing a working-coloured dot", () => {
    // The switcher had reached past the shared indicator to the dot underneath,
    // so the one surface you meet by keyboard was the one saying "working" as a
    // state while every other surface said it as a process.
    const switcherRow = (attention: "working" | "blocked") => renderToStaticMarkup(<WorkspaceSwitcher
      onClose={noop} onSelect={noop} stateGlyphs={false}
      rows={[{ session, active: true, attention, unread: 0, working: attention === "working", pinned: false, agents: [], agentOverflow: 0 }]}
    />);
    expect(switcherRow("working")).toContain('<span aria-label="Agent working" class="spinner" role="img"></span>');
    expect(switcherRow("working")).not.toContain("state-dot working");
    expect(switcherRow("blocked")).toContain('class="state-dot blocked"');
  });
});

describe("idle shows nothing, on every surface that draws state", () => {
  // One rule, swept rather than spot-checked: idle is the resting state, so no
  // surface may put a mark beside a name to say an agent is doing nothing. The
  // hollow grey circle the dots used to draw for it was the loudest thing on a
  // quiet tab strip. Unknown is not idle and keeps its dashed outline: "nothing
  // has told us" is a fact worth a mark.
  //
  // Every idle indicator is asserted as the empty hidden slot rather than as an
  // absent element, because the slot is what keeps a tab the same width idle as
  // it is working — the mark appears without moving the title it sits after.
  const idleWindow = { id: "@1", sessionId: "$1", index: 0, name: "claude", active: true, layout: "", zoomed: false };
  const idleRollups = deriveAgentRollups([agent({ id: "quiet", lifecycle: "idle" })]);

  const tabStrip = (stateGlyphs: boolean) => renderToStaticMarkup(<TabStrip
    activeKey="terminal:@1" activeTerminalPaneCount={1} canMutate commandScope={commandScope}
    onClose={noop} onCloseCurrent={noop} onCloseNonAgent={noop} onCloseOthers={noop} onCloseRight={noop}
    onDownloadTab={noop} onMove={noop} onNewTerminal={noop} onPin={noop} onRenameTerminal={noop}
    onSelect={noop} onTogglePinned={noop} platform="mac" shortcuts={{}} stateGlyphs={stateGlyphs}
    tabs={combineWorkspaceTabs([idleWindow] as never, [], idleRollups.byWindow)}
  />);

  it("draws an empty, hidden slot instead of a dot, with the glyph option on or off", () => {
    for (const stateGlyphs of [false, true]) {
      const html = tabStrip(stateGlyphs);
      expect(html).toContain('<span aria-hidden="true" class="tab-dot idle"></span>');
      // No glyph, no label, no title: an invisible box that announced "Agent
      // idle" would be the same noise moved into the accessibility tree.
      expect(html).not.toContain("Agent idle");
    }
    // The sidebar's Idle heading keeps the word and loses the circle — the text
    // is the label, and the dot beside it was saying it a second time. The slot
    // stays, so the four headings' words still line up with each other.
    const heading = sidebar({ agents: rowsFor([agent({ id: "quiet", lifecycle: "idle" })]) });
    expect(heading).toContain("<span>Idle</span>");
    expect(heading).toContain('<span aria-hidden="true" class="state-dot agent-group-dot idle"></span>');
    // The workspace switcher's rows share the same dot and the same rule.
    const switcher = renderToStaticMarkup(<WorkspaceSwitcher
      onClose={noop} onSelect={noop} stateGlyphs={false}
      rows={[{ session, active: true, attention: "idle", unread: 0, working: false, pinned: false, agents: [], agentOverflow: 0 }]}
    />);
    expect(switcher).toContain('<span aria-hidden="true" class="state-dot idle"></span>');
  });

  it("hides the slot in the stylesheet, and leaves unknown its outline", () => {
    // `visibility`, not `display: none` — the box has to survive being hidden
    // or the slot it is reserving disappears with it.
    expect(stylesCss).toContain(".state-dot.idle, .state-dot.none, .tab-dot.idle { visibility: hidden; }");
    // The idle circle is gone from every mode, forced colors included, where it
    // used to come back as a dashed border.
    expect(stylesCss).not.toMatch(/\.(state|tab)-dot\.idle[^{]*\{[^}]*border/);
    // Unknown is a different state and still draws.
    expect(stylesCss).toContain(".state-dot.unknown { background: transparent; border: 1.5px dashed var(--state-unknown); }");
    expect(stylesCss).toContain(".tab-dot.unknown { background: transparent; border: 1px dashed var(--state-unknown); }");
  });
});

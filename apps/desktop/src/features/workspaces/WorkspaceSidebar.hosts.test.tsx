// @vitest-environment jsdom
import { renderToStaticMarkup } from "react-dom/server";
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import stylesCss from "../../styles.css?raw";
import type { Session } from "../../app/types";
import { buildAgentRows } from "../agents/agentsList";
import { agent } from "../agents/testFixtures";
import type { HostScopeToken } from "../shell/hostScope";
import type { MergedWorkspaceRow } from "./mergedWorkspaceRows";
import { WorkspaceSidebar, type SidebarHost } from "./WorkspaceSidebar";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const noop = vi.fn();
const scopeFor = (hostProfileId: string): HostScopeToken =>
  ({ hostProfileId, connectionKey: `ssh:${hostProfileId}`, connectionEpoch: 1, serverIdentity: `server-${hostProfileId}`, generation: 1 });

const local: SidebarHost = {
  profileId: "local", letter: "L", label: "Local", transport: "local", phase: "connected", canMutate: true,
  scope: scopeFor("local"), active: true, shown: true, latencyMs: 3,
};
const peer: SidebarHost = {
  profileId: "peer", letter: "P", label: "peer-box", transport: "ssh", phase: "reconnecting", canMutate: false,
  scope: scopeFor("peer"), active: false, shown: true,
};
const hidden: SidebarHost = {
  profileId: "spare", letter: "S", label: "spare", transport: "ssh", phase: "disconnected", canMutate: false,
  scope: scopeFor("spare"), active: false, shown: false,
};

const session = (id: string, name: string): Session => ({ id, name, windowCount: 1, attachedClients: 0, order: 0 });
const row = (host: SidebarHost, id: string, name: string, letter = host.letter): MergedWorkspaceRow => ({
  session: session(id, name), active: host.active && id === "$0", attention: "none", unread: 0, working: false, pinned: false,
  agents: [], agentOverflow: 0,
  key: `${host.profileId}\0${id}`, hostProfileId: host.profileId, letter, scope: host.scope, phase: host.phase, canMutate: host.canMutate,
});

const props = (overrides: Partial<Parameters<typeof WorkspaceSidebar>[0]> = {}): Parameters<typeof WorkspaceSidebar>[0] => ({
  adapters: [], agents: [], agentSort: "workspace", agentsRatio: 0.4, compactWorkspaces: false, hosts: [local, peer, hidden],
  maxWidth: 426, onAgentsRatio: noop, onLaunchAgent: noop, onOpenSettings: noop, onRenameAgent: noop, onResumeAgent: noop,
  onReviewHooks: noop, onSelectAgent: noop, onSelectWorkspace: noop, onSortMode: noop, onTogglePinnedAgentTab: noop,
  onTogglePinnedOnly: noop, onTogglePinnedWorkspace: noop, onToggleShown: noop, onWidth: noop, onWorkspaceCommand: noop,
  pinnedOnly: false, rows: [row(local, "$0", "alpha"), row(peer, "$0", "delta")], stateGlyphs: false, width: 240,
  ...overrides,
});
const sidebar = (overrides: Partial<Parameters<typeof WorkspaceSidebar>[0]> = {}) =>
  renderToStaticMarkup(<WorkspaceSidebar {...props(overrides)} />);

const LETTER = (letter: string) => `<span aria-hidden="true" class="host-letter">${letter}</span>`;

describe("the host letter", () => {
  it("sits between the shortcut index and the name, and names the host in the row's label", () => {
    const html = sidebar();
    expect(html).toContain(`<span class="workspace-title"><span aria-hidden="true" class="workspace-shortcut-index">1</span>${LETTER("L")}`);
    expect(html).toContain(`<span aria-hidden="true" class="workspace-shortcut-index">2</span>${LETTER("P")}`);
    expect(html).toContain('aria-label="alpha, on Local"');
    // The peer is reconnecting: its row says so, and is drawn dimmed.
    expect(html).toContain('aria-label="delta, on peer-box, host reconnecting" class="workspace-button offline"');
    // Two rows with the same session id on two hosts are two list items.
    expect(html.match(/class="workspace-row"/g)).toHaveLength(2);
  });

  it("draws nothing, and says nothing, when the rows carry no letter", () => {
    const html = sidebar({ rows: [row(local, "$0", "alpha", "")] });
    expect(html).not.toContain("host-letter");
    expect(html).toContain('aria-label="alpha"');
  });

  it("marks agent rows the same way, in front of the name", () => {
    const agents = buildAgentRows(
      [agent({ id: "a", hostProfileId: "peer", displayName: "Codex one", windowName: "Review" })],
      () => ({ workspaceOrder: 0, workspaceName: "delta", hostLabel: "peer-box", hostLetter: "P" }),
      () => true, "workspace",
    );
    const html = sidebar({ agents });
    const line = html.slice(html.indexOf('class="agent-line"'), html.indexOf('class="agent-name-cell"'));
    expect(line).toContain(LETTER("P"));
    expect(sidebar({ agents: agents.map((item) => ({ ...item, location: { ...item.location, hostLetter: "" } })), rows: [] }))
      .not.toContain("host-letter");
  });

  it("marks ⌘P rows before the title", () => {
    const html = renderToStaticMarkup(<WorkspaceSwitcher onClose={noop} onSelect={noop} rows={[row(peer, "$0", "delta")]} stateGlyphs={false} />);
    // Announced, not hidden: it is the only host cue a ⌘P row has.
    expect(html).toContain('<span class="host-letter">P</span><span class="palette-title">delta</span>');
    // The row key holds a NUL; the DOM id must not.
    expect(html).not.toContain("\0");
    expect(html).toContain('id="workspace-option-peer%00%240"');
  });

  it("takes the shortcut index's slot and flips to the knockout ink on the active row", () => {
    expect(stylesCss).toMatch(/\.host-letter \{\n\s+width: 12px; flex: 0 0 auto;/);
    expect(stylesCss).toContain(".workspace-button.active .host-letter { color: var(--accent-ink); }");
    expect(stylesCss).toContain(".palette-row.selected:not([data-unavailable]) .host-letter { color: var(--accent-ink); }");
  });
});

describe("rows act on their own host", () => {
  it("hands the row over whole, host scope included", async () => {
    const onSelectWorkspace = vi.fn();
    const onTogglePinnedWorkspace = vi.fn();
    const onWorkspaceCommand = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => {
      renderer = create(<WorkspaceSidebar {...props({ onSelectWorkspace, onTogglePinnedWorkspace, onWorkspaceCommand })} />);
    });
    const peerRow = renderer.root.findByProps({ "data-workspace-index": 1 });
    await act(async () => peerRow.props.onClick({ shiftKey: false }));
    expect(onSelectWorkspace).toHaveBeenCalledWith(expect.objectContaining({ key: "peer\0$0", scope: peer.scope }));
    await act(async () => peerRow.props.onClick({ shiftKey: true }));
    expect(onTogglePinnedWorkspace).toHaveBeenCalledWith(expect.objectContaining({ key: "peer\0$0" }));

    // The peer is read-only: its row's mutations are off while pin stays on,
    // and the local row beside it is untouched by that.
    await act(async () => peerRow.props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    expect(renderer.root.findByProps({ "data-menu-item": "rename" }).props.disabled).toBe(true);
    expect(renderer.root.findByProps({ "data-menu-item": "close" }).props.disabled).toBe(true);
    expect(renderer.root.findByProps({ "data-menu-item": "pin" }).props.disabled).toBeUndefined();
    await act(async () => renderer.root.findByProps({ "data-menu-item": "pin" }).props.onClick());
    expect(onTogglePinnedWorkspace).toHaveBeenCalledTimes(2);

    await act(async () => peerRow.props.onDoubleClick());
    expect(onWorkspaceCommand).not.toHaveBeenCalled();
    const localRow = renderer.root.findByProps({ "data-workspace-index": 0 });
    await act(async () => localRow.props.onDoubleClick());
    expect(onWorkspaceCommand).toHaveBeenCalledWith(expect.objectContaining({ key: "local\0$0" }), "session.rename");
    await act(async () => renderer.unmount());
  });

  it("stops offering the pin, and takes the mutations, once the connection under the menu changes", async () => {
    const onTogglePinnedWorkspace = vi.fn();
    const element = (host: SidebarHost) => <WorkspaceSidebar {...props({ hosts: [host], onTogglePinnedWorkspace, rows: [row(host, "$0", "alpha")] })} />;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(element(local)); });
    await act(async () => renderer.root.findByProps({ "data-workspace-index": 0 }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    expect(renderer.root.findAllByProps({ "data-menu-item": "pin" })).toHaveLength(1);
    expect(renderer.root.findByProps({ "data-menu-item": "rename" }).props.disabled).toBe(false);

    // Read-only now: the row is still this row, so the pin stays and the
    // mutations go.
    await act(async () => { renderer.update(element({ ...local, canMutate: false })); });
    expect(renderer.root.findAllByProps({ "data-menu-item": "pin" })).toHaveLength(1);
    expect(renderer.root.findByProps({ "data-menu-item": "rename" }).props.disabled).toBe(true);

    // Reconnected to another server that reuses the id: not this row any more,
    // and nothing can be asked of a row that is not there.
    await act(async () => { renderer.update(element({ ...local, scope: { ...local.scope, connectionEpoch: 2, serverIdentity: "server-b" } })); });
    expect(renderer.root.findAllByProps({ "data-menu-item": "pin" })).toHaveLength(0);
    expect(renderer.root.findByProps({ "data-menu-item": "rename" }).props.disabled).toBe(true);
    expect(renderer.root.findByProps({ "data-menu-item": "close" }).props.disabled).toBe(true);
    expect(onTogglePinnedWorkspace).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("bounds a row's moves by its own host's list", async () => {
    const rows = [row(local, "$0", "alpha"), { ...row(local, "$1", "beta"), session: { ...session("$1", "beta"), order: 1 } }, row(peer, "$0", "delta")];
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<WorkspaceSidebar {...props({ rows })} />); });
    // The last local row is last among locals, even though a peer row follows it on screen.
    await act(async () => renderer.root.findByProps({ "data-workspace-index": 1 }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    expect(renderer.root.findByProps({ "data-menu-item": "up" }).props.disabled).toBe(false);
    expect(renderer.root.findByProps({ "data-menu-item": "down" }).props.disabled).toBe(true);
    await act(async () => renderer.unmount());
  });

  it("looks an agent row's scope up by its host, and falls back to the active host", async () => {
    const onSelectAgent = vi.fn();
    const agents = buildAgentRows(
      [
        agent({ id: "on-peer", hostProfileId: "peer", paneId: "%1", displayName: "peer agent" }),
        agent({ id: "orphan", hostProfileId: "gone", paneId: "%2", displayName: "orphan agent" }),
      ],
      (record) => ({ workspaceOrder: record.hostProfileId === "peer" ? 0 : 1, workspaceName: "delta" }),
      () => true, "workspace",
    );
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<WorkspaceSidebar {...props({ agents, onSelectAgent })} />); });
    await act(async () => renderer.root.findByProps({ "data-agent-index": 0 }).props.onClick({ shiftKey: false }));
    expect(onSelectAgent).toHaveBeenLastCalledWith(expect.objectContaining({ agent: expect.objectContaining({ id: "on-peer" }) }), peer.scope);
    await act(async () => renderer.root.findByProps({ "data-agent-index": 1 }).props.onClick({ shiftKey: false }));
    expect(onSelectAgent).toHaveBeenLastCalledWith(expect.objectContaining({ agent: expect.objectContaining({ id: "orphan" }) }), local.scope);

    // Rename on the read-only peer is off; the same item on the local host is on.
    await act(async () => renderer.root.findByProps({ "data-agent-index": 0 }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    expect(renderer.root.findByProps({ "data-menu-item": "rename" }).props.disabled).toBe(true);
    await act(async () => renderer.root.findByProps({ "data-agent-index": 1 }).props.onContextMenu({ preventDefault: noop, clientX: 10, clientY: 10 }));
    expect(renderer.root.findByProps({ "data-menu-item": "rename" }).props.disabled).toBe(false);
    await act(async () => renderer.unmount());
  });
});

describe("the host row", () => {
  it("shows the active host as it always did", () => {
    const html = sidebar();
    expect(html).toContain('aria-haspopup="menu" aria-label="Host Local over local, connected. Open the host menu." class="host-row"');
    expect(html).toContain('<span class="host-name">Local</span><span class="host-transport">local</span><span class="host-latency">3 ms</span>');
    expect(html).toContain('class="link-dot connected"');
    expect(html).not.toContain("peer-box</span>");
  });

  it("opens a menu with one checked item per host and the way into settings", async () => {
    const onToggleShown = vi.fn();
    const onOpenSettings = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<WorkspaceSidebar {...props({ onOpenSettings, onToggleShown })} />); });
    const hostRow = renderer.root.findByProps({ className: "host-row" });
    expect(hostRow.props["aria-expanded"]).toBe(false);
    await act(async () => hostRow.props.onClick({ currentTarget: { getBoundingClientRect: () => ({ left: 0, bottom: 0 }) } }));
    expect(renderer.root.findByProps({ className: "host-row" }).props["aria-expanded"]).toBe(true);

    const items = renderer.root.findAllByProps({ role: "menuitemradio" });
    expect(items.map((item) => item.props["data-menu-item"])).toEqual(["host-local", "host-peer", "host-spare"]);
    expect(items.map((item) => item.props["aria-checked"])).toEqual([true, true, false]);
    // The active host is always shown: checked, and not offered for unchecking.
    expect(items.map((item) => item.props.disabled)).toEqual([true, false, false]);
    const labels = items.map((item) => item.findByProps({ className: "menu-item-label" }).children.at(-1));
    expect(labels).toEqual(["L Local", "P peer-box", "S spare"]);

    await act(async () => items[2].props.onClick());
    expect(onToggleShown).toHaveBeenCalledWith("spare");
    // Choosing closes the menu, as every menu does.
    expect(renderer.root.findAllByProps({ role: "menu" })).toHaveLength(0);

    await act(async () => renderer.root.findByProps({ className: "host-row" }).props.onClick({ currentTarget: { getBoundingClientRect: () => ({ left: 0, bottom: 0 }) } }));
    const settings = renderer.root.findByProps({ "data-menu-item": "settings" });
    expect(settings.props.role).toBe("menuitem");
    await act(async () => settings.props.onClick());
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    await act(async () => renderer.unmount());
  });
});

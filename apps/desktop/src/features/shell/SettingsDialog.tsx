import { useId, useState } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import { SurfaceError } from "../../ui/SurfaceError";
import type { HostProfile } from "../../app/types";
import type { AgentSoundPreferences } from "../agents/types";
import type { HelperUpgradeState, RemoteHelperProbe } from "./helperUpgrade";
import type { ShellState } from "./types";

interface SettingsDialogProps {
  connectionMode: "local" | "ssh";
  profiles: readonly HostProfile[];
  /** The picked saved host; empty is the "Current values" entry. */
  selectedProfileId: string;
  /** The saved host Delete would remove, or undefined while there is none. */
  deletableProfile?: HostProfile;
  sshTarget: string;
  sshConfigPath: string;
  helper: HelperUpgradeState;
  remote: boolean;
  shell: ShellState;
  sounds: AgentSoundPreferences;
  onClose(): void;
  onConnect(): void;
  onConnectionMode(mode: "local" | "ssh"): void;
  onDeleteProfile(): void;
  onProfile(profile: HostProfile | undefined): void;
  onSshTarget(value: string): void;
  onSshConfigPath(value: string): void;
  onProbeHelper(): void;
  onRequestHelperInstall(): void;
  onShell(update: Partial<ShellState>): void;
  onSounds(preferences: AgentSoundPreferences): void;
  /**
   * Where a "not now" is taken back. The one-time prompt is deliberately
   * one-time, so declining it has to leave a way back that is not "reinstall
   * the app" — and someone whose hooks were later removed by another tool
   * needs the same door.
   */
  agentSetup: { available: boolean; reports: boolean; onSetUp(): void };
}

type SettingsTab = "connection" | "sounds" | "accessibility";

const TABS: readonly { id: SettingsTab; label: string }[] = [
  { id: "connection", label: "Connection" },
  { id: "sounds", label: "Sounds" },
  { id: "accessibility", label: "Accessibility" },
];

/**
 * Where the docked panels went.
 *
 * Two surfaces used to sit permanently in the shell taking space from the
 * terminal on every window: a connection card with an SSH form, and a sounds
 * mixer with four controls. Neither is something anyone touches twice a
 * session. They are here, behind ⌘, and behind the sidebar's host row.
 *
 * The accessibility tab is not decoration: it is where the terminal
 * screen-reader toggle Phase 12 deferred finally lives (P12-U002), together
 * with the shape-glyph option that makes the agent state dots readable without
 * relying on color.
 */
export function SettingsDialog(props: SettingsDialogProps) {
  const [tab, setTab] = useState<SettingsTab>("connection");
  const titleId = useId();
  const settingsPanelId = useId();
  const dialog = useModalDialog<HTMLElement>(props.onClose);

  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.currentTarget === event.target) props.onClose();
  }}>
    <section aria-labelledby={titleId} aria-modal="true" className="settings-dialog" ref={dialog} role="dialog">
      <header>
        <h2 id={titleId}>Settings</h2>
        <div aria-label="Settings sections" className="segmented" role="tablist">
          {TABS.map((item) => <button
            aria-controls={tab === item.id ? settingsPanelId : undefined}
            aria-selected={tab === item.id}
            className={tab === item.id ? "segment active" : "segment"}
            id={`settings-tab-${item.id}`}
            key={item.id}
            onClick={() => setTab(item.id)}
            // Arrow keys move the selection *and* the focus with it. Leaving
            // focus on a tab that now reports `aria-selected={false}` is how a
            // screen-reader user ends up being told they are on a tab that is
            // not the one showing.
            onKeyDown={(event) => {
              const delta = event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
              if (!delta) return;
              event.preventDefault();
              const index = TABS.findIndex((candidate) => candidate.id === tab);
              const next = TABS[(index + delta + TABS.length) % TABS.length].id;
              setTab(next);
              window.requestAnimationFrame(() => document.getElementById(`settings-tab-${next}`)?.focus());
            }}
            role="tab"
            tabIndex={tab === item.id ? 0 : -1}
            type="button"
          >{item.label}</button>)}
        </div>
      </header>

      <div
        aria-labelledby={`settings-tab-${tab}`}
        className="settings-body"
        id={settingsPanelId}
        role="tabpanel"
        tabIndex={0}
      >
        {tab === "connection" && <>
          <label>Saved host
            {/* Bound to what the user picked, not to what the app is connected
                to. Connect is what turns one into the other. */}
            <select aria-label="Saved host" onChange={(event) => {
              const profile = props.profiles.find((item) => item.id === event.target.value);
              props.onProfile(profile);
            }} value={props.selectedProfileId}>
              <option value="">Current values (not saved)</option>
              {props.profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.label}</option>)}
            </select>
          </label>
          {/* Under the picker on its own line, not beside it: this is the one
              control in the panel that destroys something, and putting it a
              few pixels from the control the user reaches for most is how it
              gets pressed by accident. It names its subject for the same
              reason — "Delete host…" beside a combobox is a verb with no
              object. */}
          <div className="settings-host-actions">
            <button
              className="danger-quiet"
              disabled={!props.deletableProfile}
              onClick={props.onDeleteProfile}
              type="button"
            >{props.deletableProfile ? `Delete “${props.deletableProfile.label}”…` : "Delete saved host…"}</button>
            {/* A disabled control that does not say why reads as broken, and
                the picker is showing a host, so "why can't I delete this one?"
                is the obvious question. */}
            <span className="settings-hint">{hostDeleteHint(props)}</span>
          </div>
          <div className="settings-host-actions">
            <button disabled={!props.agentSetup.available} onClick={props.agentSetup.onSetUp} type="button">Set up agent status…</button>
            <span className="settings-hint">{agentSetupHint(props.agentSetup)}</span>
          </div>
          <fieldset className="settings-modes">
            <legend>Transport</legend>
            <label><input checked={props.connectionMode === "local"} name="connection-mode" onChange={() => props.onConnectionMode("local")} type="radio" /> Local</label>
            <label><input checked={props.connectionMode === "ssh"} name="connection-mode" onChange={() => props.onConnectionMode("ssh")} type="radio" /> SSH</label>
          </fieldset>
          {props.connectionMode === "ssh" && <>
            <label>SSH host<input onChange={(event) => props.onSshTarget(event.target.value)} placeholder="Host or config alias" value={props.sshTarget} /></label>
            <label>SSH config<input onChange={(event) => props.onSshConfigPath(event.target.value)} placeholder="Optional path" value={props.sshConfigPath} /></label>
          </>}
          {props.remote && <div className="settings-helper">
            <button disabled={props.helper.phase === "probing" || props.helper.phase === "upgrading"} onClick={props.onProbeHelper} type="button">
              {props.helper.phase === "probing" ? "Checking helper…" : "Check helper"}
            </button>
            {helperCanInstall(props.helper) && <button className="primary" onClick={props.onRequestHelperInstall} type="button">
              {props.helper.probe.installed ? "Upgrade helper…" : "Install helper…"}
            </button>}
            {props.helper.phase === "ready" && <HelperDetails probe={props.helper.probe} />}
            {props.helper.phase === "upgrading" && <p role="status">Upgrading the remote helper; the previous one is retained until the new handshake succeeds.</p>}
            {props.helper.phase === "failed" && <SurfaceError
              detail={props.helper.message}
              summary={props.helper.rollback === "restored" ? "Upgrade failed; the previous helper was restored." : props.helper.rollback === "failed" ? "Upgrade and rollback both failed — check the helper on the host before reconnecting." : "The helper check failed."}
            />}
            {props.helper.phase === "succeeded" && <p role="status">Helper {props.helper.operation === "install" ? "installed" : "upgraded"}. {props.helper.message}</p>}
          </div>}
        </>}

        {tab === "sounds" && <>
          <label className="settings-check"><input checked={props.sounds.enabled} onChange={(event) => props.onSounds({ ...props.sounds, enabled: event.target.checked })} type="checkbox" /> Play agent cues</label>
          <label className="settings-check"><input checked={props.sounds.blocked !== "none"} disabled={!props.sounds.enabled} onChange={(event) => props.onSounds({ ...props.sounds, blocked: event.target.checked ? "subtle" : "none" })} type="checkbox" /> Cue when an agent is blocked</label>
          <label className="settings-check"><input checked={props.sounds.completed !== "none"} disabled={!props.sounds.enabled} onChange={(event) => props.onSounds({ ...props.sounds, completed: event.target.checked ? "subtle" : "none" })} type="checkbox" /> Cue when an agent finishes</label>
          <label>Volume<input aria-label="Agent sound volume" max="1" min="0" onChange={(event) => props.onSounds({ ...props.sounds, volume: Number(event.target.value) })} step="0.05" type="range" value={props.sounds.volume} /></label>
        </>}

        {tab === "accessibility" && <>
          <label className="settings-check">
            <input checked={props.shell.agentStateGlyphs} onChange={(event) => props.onShell({ agentStateGlyphs: event.target.checked })} type="checkbox" />
            Shapes in agent state dots
          </label>
          <p className="settings-hint">Draws a symbol inside each dot as well as coloring it, so agent state does not depend on telling red from yellow.</p>
          <label className="settings-check">
            <input checked={props.shell.terminalScreenReader} onChange={(event) => props.onShell({ terminalScreenReader: event.target.checked })} type="checkbox" />
            Expose terminal contents to screen readers
          </label>
          <p className="settings-hint">
            Mirrors every terminal row into the accessibility tree. It costs real throughput on a pane producing continuous output, so it is off unless you need it. Panes pick this up as they are re-created; reconnect to apply it everywhere.
          </p>
        </>}
      </div>

      <footer><button className="primary" onClick={tab === "connection" ? props.onConnect : props.onClose} type="button">{tab === "connection" ? "Connect" : "Done"}</button></footer>
    </section>
  </div>;
}

/** What the host's agent configuration is, in the words of what it is. */
function agentSetupHint(setup: SettingsDialogProps["agentSetup"]): string {
  if (setup.available) return "Adds this app’s lifecycle hooks alongside the ones already configured on this host.";
  if (setup.reports) return "This host already reports agent status.";
  return "Connect to a host to check whether it can report agent status.";
}

/** Why Delete is unavailable, in the words of the reason it is unavailable. */
function hostDeleteHint(props: Pick<SettingsDialogProps, "deletableProfile" | "profiles" | "selectedProfileId">): string {
  if (props.deletableProfile) return "Removes it from this machine only. The host itself is not touched.";
  if (props.profiles.length <= 1) return "The last saved host cannot be removed.";
  return "Pick a saved host above to remove it.";
}

function helperCanInstall(state: HelperUpgradeState): state is Extract<HelperUpgradeState, { phase: "ready" }> {
  return state.phase === "ready" && !state.probe.compatible;
}

function HelperDetails({ probe }: { probe: RemoteHelperProbe }) {
  return <dl className="helper-details">
    <dt>Remote</dt><dd>{probe.operatingSystem} {probe.architecture}</dd>
    <dt>tmux</dt><dd>{probe.tmuxVersion}</dd>
    <dt>Helper</dt><dd>{probe.installed ? probe.helperVersion ?? "unreadable" : "not installed"}</dd>
    <dt>Path</dt><dd>{probe.remotePath}</dd>
  </dl>;
}

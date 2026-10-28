import type { ComponentType } from "react";
import { AutomationsPanel } from "../automations/AutomationsPanel";

/** A shown host, as a panel extension sees it. */
export interface PanelHost {
  profileId: string;
  label: string;
  /** The live bridge's client; absent until the host has connected. */
  clientId?: string;
  connected: boolean;
  /** The tmux server the host is showing now, once it has said. */
  serverIdentity?: string;
  /** Whether a pane is open on that server now. */
  hasPane(paneId: string): boolean;
  /** Settings' "Start in directory" for new workspaces on this host, if set. */
  startDirectory?: string;
}

export interface PanelExtensionProps {
  hosts: readonly PanelHost[];
  activeProfileId: string;
  /** Brings a pane on screen, on whichever host it is. */
  onOpenPane(target: { profileId: string; sessionId: string; paneId: string; source: string }): void;
  onMessage(message: string): void;
}

/**
 * An optional segment of the right panel, beside Files, Git and Ports. It is
 * hidden until turned on in Settings, Workspace tab; adding one is an entry
 * here and nothing else.
 */
export interface PanelExtension {
  id: string;
  label: string;
  settingLabel: string;
  settingHint: string;
  Component: ComponentType<PanelExtensionProps>;
}

export const PANEL_EXTENSIONS: readonly PanelExtension[] = [
  {
    id: "automations",
    label: "Automations",
    settingLabel: "Show Automations",
    settingHint: "Adds an Automations tab to the right panel: agent runs on a cron schedule, owned by each host's helper.",
    Component: AutomationsPanel,
  },
];

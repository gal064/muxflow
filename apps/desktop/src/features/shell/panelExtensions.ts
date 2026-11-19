import type { ComponentType } from "react";
import type { ConnectionSpec, TmuxSnapshot } from "../../app/types";
import type { AgentRecord } from "../agents/types";
import type { AiChatSettings } from "../ai/types";
import { AiPanel } from "../ai/AiPanel";
import { AutomationsPanel } from "../automations/AutomationsPanel";
import type { GitStatusSnapshot } from "../git/types";
import type { PortForward } from "../ports/api";

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

/**
 * What the shell is showing, handed to extensions that read the session
 * rather than one domain. It is a live view — rebuilt per render from the
 * shell's own state — so a consumer must never hold a piece of it past the
 * render it came from.
 */
export interface PanelSessionContext {
  hostLabel: string;
  transport: "local" | "ssh";
  connected: boolean;
  /** The live tmux view of the host on screen. */
  snapshot: TmuxSnapshot;
  activeSessionId?: string;
  activeWindowId?: string;
  activePaneId?: string;
  /** The host's coding agents, as its hooks reported them. */
  agents: readonly AgentRecord[];
  /** The document tabs open in the current workspace. */
  openTabs: readonly { kind: string; title: string; resource: string }[];
  /** The Explorer's root path, when the active pane has one. */
  activeRootPath?: string;
  gitStatus?: GitStatusSnapshot;
  gitError?: string;
  /** Forwards the native side owns, for the host on screen. */
  portForwards: readonly PortForward[];
  /** The SSH connection, for hosts where port detection can run. */
  sshConnection?: Extract<ConnectionSpec, { mode: "ssh" }>;
  /** Settings → Assistant: the endpoint the AI tab will call, if configured. */
  assistant: AiChatSettings;
}

export interface PanelExtensionProps {
  hosts: readonly PanelHost[];
  activeProfileId: string;
  /** Brings a pane on screen, on whichever host it is. */
  onOpenPane(target: { profileId: string; sessionId: string; paneId: string; source: string }): void;
  onMessage(message: string): void;
  /**
   * What the shell is showing, for extensions that answer questions about it.
   * Optional so the extensions that speak to one domain ignore it entirely.
   */
  session?: PanelSessionContext;
}

/**
 * An optional segment of the right panel, beside Files, Git and Ports. It is
 * turned on or off in Settings, Workspace tab; `defaultShellState` says which
 * ones start on. Adding one is an entry here and nothing else.
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
  {
    id: "ai",
    label: "AI",
    settingLabel: "Show AI assistant",
    settingHint: "Adds an AI tab to the right panel: ask an OpenAI-compatible model about the current session; it is shown what the Files, Git, Ports and Automations tabs hold.",
    Component: AiPanel,
  },
];

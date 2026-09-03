import { CommandPalette } from "../commands/CommandPalette";
import { ConfirmationDialog } from "../commands/ConfirmationDialog";
import { ShortcutEditorDialog } from "../commands/ShortcutEditorDialog";
import { TextInputDialog, type PendingTextPrompt } from "../commands/TextInputDialog";
import { NewWorkspaceDialog, type NewWorkspaceHostOption } from "../commands/NewWorkspaceDialog";
import type { PendingTmuxConfirmation } from "../commands/destructiveConfirmation";
import type { CommandContext, CommandId, Platform, ShortcutOverrides } from "../commands/registry";
import type { HelperUpgradeState } from "../features/shell/helperUpgrade";
import type { HostProfile } from "./types";
import type { AppRecoveryDiscardState } from "./appRecovery";

type AppDialogLayerProps = {
  appRecoveryDiscard?: AppRecoveryDiscardState;
  appStateResetConfirmation: boolean;
  confirmation?: PendingTmuxConfirmation;
  /** The saved host `host.delete` is asking about, if it asked. */
  hostDelete?: HostProfile;
  helperState: HelperUpgradeState;
  paletteOpen: boolean;
  platform: Platform;
  profileResetConfirmation: boolean;
  shortcuts: ShortcutOverrides;
  shortcutEditorOpen: boolean;
  textPrompt?: PendingTextPrompt;
  newWorkspace?: { hosts: readonly NewWorkspaceHostOption[]; selectedHostProfileId: string };
  commandContext: CommandContext;
  onAppRecoveryDiscardCancel(): void;
  onAppRecoveryDiscardConfirm(): void;
  onAppStateResetCancel(): void;
  onAppStateResetConfirm(): void;
  onConfirmationCancel(): void;
  onConfirmationConfirm(confirmation: PendingTmuxConfirmation): void;
  onHelperCancel(): void;
  onHelperConfirm(): void;
  onHostDeleteCancel(): void;
  onHostDeleteConfirm(profile: HostProfile): void;
  onNewWorkspaceCancel(): void;
  onNewWorkspaceHost(profileId: string): void;
  onNewWorkspaceSubmit(name: string, hostProfileId: string): void;
  onPaletteClose(): void;
  onProfileResetCancel(): void;
  onProfileResetConfirm(): void;
  onRunCommand(id: CommandId): void;
  onShortcutChange(shortcuts: ShortcutOverrides): void;
  onShortcutClose(): void;
  onTextPromptCancel(): void;
};

export function AppDialogLayer(props: AppDialogLayerProps) {
  const {
    appRecoveryDiscard, appStateResetConfirmation, commandContext, confirmation,
    helperState, hostDelete, newWorkspace, paletteOpen, platform, profileResetConfirmation,
    shortcuts, shortcutEditorOpen, textPrompt,
  } = props;
  return <>
    {paletteOpen && <CommandPalette context={commandContext} onClose={props.onPaletteClose} onInvoke={props.onRunCommand} platform={platform} shortcuts={shortcuts} />}
    {shortcutEditorOpen && <ShortcutEditorDialog onChange={props.onShortcutChange} onClose={props.onShortcutClose} overrides={shortcuts} platform={platform} />}
    {textPrompt && <TextInputDialog {...textPrompt} onCancel={props.onTextPromptCancel} />}
    {newWorkspace && <NewWorkspaceDialog
      hosts={newWorkspace.hosts}
      onCancel={props.onNewWorkspaceCancel}
      onHost={props.onNewWorkspaceHost}
      onSubmit={props.onNewWorkspaceSubmit}
      selectedHostProfileId={newWorkspace.selectedHostProfileId}
    />}
    {confirmation && <ConfirmationDialog destructive detail={confirmation.detail} title={confirmation.title} onCancel={props.onConfirmationCancel} onConfirm={() => props.onConfirmationConfirm(confirmation)} />}
    {appStateResetConfirmation && <ConfirmationDialog
      confirmLabel="Preserve and reset"
      destructive={false}
      detail="The incompatible or malformed file will be preserved beside app-state.json, then shell preferences and app-owned tabs will start empty. tmux sessions and terminal processes are not changed."
      title="Reset saved shell state?"
      onCancel={props.onAppStateResetCancel}
      onConfirm={props.onAppStateResetConfirm}
    />}
    {profileResetConfirmation && <ConfirmationDialog
      confirmLabel="Use recovered defaults"
      destructive={false}
      detail="Keep the preserved invalid profile file and write a fresh Local profile. tmux sessions and terminal processes are not changed."
      title="Reset saved host profiles?"
      onCancel={props.onProfileResetCancel}
      onConfirm={props.onProfileResetConfirm}
    />}
    {hostDelete && <ConfirmationDialog
      confirmLabel="Delete host"
      destructive
      detail={`“${hostDelete.label}” will be removed from the saved hosts on this machine. Nothing on the host is changed: its tmux sessions, its helper and your SSH config are untouched, and connecting to it again saves it again.`}
      title="Delete saved host?"
      onCancel={props.onHostDeleteCancel}
      onConfirm={() => props.onHostDeleteConfirm(hostDelete)}
    />}
    {appRecoveryDiscard && <ConfirmationDialog
      confirmLabel="Discard old tabs"
      destructive
      detail={`${appRecoveryDiscard.count} app-owned tab record${appRecoveryDiscard.count === 1 ? "" : "s"} from the replaced server will be removed. tmux sessions and terminal processes are not changed.`}
      title="Discard app tabs from replaced server?"
      onCancel={props.onAppRecoveryDiscardCancel}
      onConfirm={props.onAppRecoveryDiscardConfirm}
    />}
    {helperState.phase === "confirming" && <ConfirmationDialog
      confirmLabel={helperState.probe.installed ? "Upgrade and reconnect" : "Install and connect"}
      destructive={false}
      // One consent for one setup. A first install used to be followed, a few
      // seconds later, by a second dialog asking to set up agent status on the
      // same host the user had just agreed to set up — two questions about one
      // decision, the second one arriving after the first had visibly
      // succeeded. It is named here instead, so what is agreed to is what
      // happens. An upgrade says nothing about agents: that host answered the
      // agent question long ago, and this dialog is not where it changes.
      detail={helperState.probe.installed
        ? `Replace the helper at ${helperState.probe.remotePath}. The current helper is backed up and restored automatically if the new helper cannot complete its handshake.`
        : `Install the packaged helper at ${helperState.probe.remotePath}, verify its digest and handshake, then connect to tmux. Agent status hooks for the agents found on this host will also be set up after connecting.`}
      title={helperState.probe.installed ? "Upgrade remote helper?" : "Install remote helper?"}
      onCancel={props.onHelperCancel}
      onConfirm={props.onHelperConfirm}
    />}
  </>;
}

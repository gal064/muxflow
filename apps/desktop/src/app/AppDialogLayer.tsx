import { CommandPalette } from "../commands/CommandPalette";
import { ConfirmationDialog } from "../commands/ConfirmationDialog";
import { ShortcutEditorDialog } from "../commands/ShortcutEditorDialog";
import { TextInputDialog, type PendingTextPrompt } from "../commands/TextInputDialog";
import type { PendingTmuxConfirmation } from "../commands/destructiveConfirmation";
import type { CommandContext, CommandId, Platform, ShortcutOverrides } from "../commands/registry";
import { DownloadDialog, type PendingDownload } from "../features/files/DownloadDialog";
import type { ActiveRoot, DownloadRequest } from "../features/files/types";
import type { HelperUpgradeState } from "../features/shell/helperUpgrade";
import type { HostProfile } from "./types";

type AppDialogLayerProps = {
  appRecoveryDiscard?: { count: number };
  appStateResetConfirmation: boolean;
  confirmation?: PendingTmuxConfirmation;
  /** The saved host `host.delete` is asking about, if it asked. */
  hostDelete?: HostProfile;
  helperState: HelperUpgradeState;
  paletteOpen: boolean;
  pendingDownload?: PendingDownload;
  platform: Platform;
  profileResetConfirmation: boolean;
  shortcuts: ShortcutOverrides;
  shortcutEditorOpen: boolean;
  textPrompt?: PendingTextPrompt;
  commandContext: CommandContext;
  onAppRecoveryDiscardCancel(): void;
  onAppRecoveryDiscardConfirm(): void;
  onAppStateResetCancel(): void;
  onAppStateResetConfirm(): void;
  onConfirmationCancel(): void;
  onConfirmationConfirm(confirmation: PendingTmuxConfirmation): void;
  onDownloadCancel(): void;
  onDownloadConfirm(request: DownloadRequest, root: ActiveRoot): void;
  onHelperCancel(): void;
  onHelperConfirm(): void;
  onHostDeleteCancel(): void;
  onHostDeleteConfirm(profile: HostProfile): void;
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
    helperState, hostDelete, paletteOpen, pendingDownload, platform, profileResetConfirmation,
    shortcuts, shortcutEditorOpen, textPrompt,
  } = props;
  return <>
    {paletteOpen && <CommandPalette context={commandContext} onClose={props.onPaletteClose} onInvoke={props.onRunCommand} platform={platform} shortcuts={shortcuts} />}
    {shortcutEditorOpen && <ShortcutEditorDialog onChange={props.onShortcutChange} onClose={props.onShortcutClose} overrides={shortcuts} platform={platform} />}
    {textPrompt && <TextInputDialog {...textPrompt} onCancel={props.onTextPromptCancel} />}
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
      detail={helperState.probe.installed
        ? `Replace the helper at ${helperState.probe.remotePath}. The current helper is backed up and restored automatically if the new helper cannot complete its handshake.`
        : `Install the packaged helper at ${helperState.probe.remotePath}, verify its digest and handshake, then connect to tmux.`}
      title={helperState.probe.installed ? "Upgrade remote helper?" : "Install remote helper?"}
      onCancel={props.onHelperCancel}
      onConfirm={props.onHelperConfirm}
    />}
    {pendingDownload && <DownloadDialog pending={pendingDownload} onCancel={props.onDownloadCancel} onConfirm={props.onDownloadConfirm} />}
  </>;
}

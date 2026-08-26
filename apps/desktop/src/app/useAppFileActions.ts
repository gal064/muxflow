import { useRef, type Dispatch, type SetStateAction } from "react";
import { keyForScope, keyForTransferConnection, sameRoot } from "../features/files/api";
import { chooseDownloadDestination, type DownloadIntent } from "../features/files/downloadFlow";
import type {
  ActiveRoot,
  DownloadRequest,
  FileMutation,
  FileWorkspaceClient,
  FileWorkspaceScope,
  TransferStatus,
} from "../features/files/types";
import { relocateFileTabs } from "../features/shell/model";
import type { PersistedAppState } from "../features/shell/types";

interface AppFileActionsOptions {
  canMutate: boolean;
  client: FileWorkspaceClient;
  currentHostProfileId: string;
  recordTransfer: (transfer: TransferStatus) => void;
  refreshDirectory: (directory: string) => void;
  root?: ActiveRoot;
  scope?: FileWorkspaceScope;
  setActiveDownloadStatus: (status: { id: string; path: string; banner: string }) => void;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
  setStatus: (status: string) => void;
}

export type DownloadOrigin = "explorer" | "fileSurface" | "tabMenu";

/** Owns filesystem mutation, native save-panel serialization, and transfer publication. */
export function useAppFileActions(options: AppFileActionsOptions) {
  const downloadPickerOpen = useRef(false);
  const scopeRef = useRef(options.scope);
  const rootRef = useRef(options.root);
  scopeRef.current = options.scope;
  rootRef.current = options.root;

  const selectionIsCurrent = (scope: FileWorkspaceScope, root: ActiveRoot, origin: DownloadOrigin) => {
    const currentScope = scopeRef.current;
    return Boolean(currentScope
      && keyForScope(currentScope) === keyForScope(scope)
      // Explorer commands act on the live tree and must remain bound to its
      // current root. A file tab carries its own host-issued root capability;
      // requiring that capability to equal the Explorer root makes a still-
      // valid tab undownloadable after `cd` changes the live tree. The host
      // validates the captured path/token pair before it opens the source.
      && (origin !== "explorer" || sameRoot(rootRef.current, root)));
  };

  const mutateFile = async (mutation: FileMutation) => {
    if (!options.scope || !options.root || !options.canMutate) {
      throw new Error("File changes are unavailable while the host is read-only.");
    }
    const mutationScope = options.scope;
    const mutationRoot = options.root;
    try {
      await options.client.mutate(mutationScope, mutationRoot, mutation);
      if (mutation.kind === "rename" || mutation.kind === "move") {
        options.setAppState((current) => relocateFileTabs(
          current,
          options.currentHostProfileId,
          mutationScope.serverIdentity,
          mutationRoot.path,
          mutation.path,
          mutation.destination,
        ));
      }
      const directory = "parent" in mutation
        ? mutation.parent
        : mutation.path.slice(0, mutation.path.lastIndexOf("/")) || mutationRoot.path;
      options.refreshDirectory(directory);
    } catch (error) {
      options.setStatus(String(error));
      throw error;
    }
  };

  const startDownload = async (
    scope: FileWorkspaceScope,
    request: DownloadRequest,
    root: ActiveRoot,
    origin: DownloadOrigin,
  ) => {
    if (!selectionIsCurrent(scope, root, origin)) {
      options.setStatus("Download cancelled because the active host or workspace changed.");
      return;
    }
    try {
      const transfer = await options.client.startDownload(scope, root, request);
      if (!selectionIsCurrent(scope, root, origin)) {
        await options.client.cancelTransfer(scope, transfer.id).catch(() => undefined);
        options.setStatus("Download cancelled because the active host or workspace changed.");
        return;
      }
      options.recordTransfer(transfer);
      const banner = `Download ${transfer.state}: ${request.path}`;
      options.setActiveDownloadStatus({ id: transfer.id, path: request.path, banner });
      options.setStatus(banner);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (!selectionIsCurrent(scope, root, origin)) {
        options.setStatus("Download cancelled because the active host or workspace changed.");
        return;
      }
      options.recordTransfer({
        id: crypto.randomUUID(),
        scopeKey: keyForTransferConnection(scope),
        path: request.path,
        destination: request.destination,
        kind: request.kind,
        state: "failed",
        outcome: "notPublished",
        failureKind: "transfer",
        completedBytes: "0",
        filesCompleted: "0",
        error: message,
      });
      options.setStatus(message);
    }
  };

  const startDownloadFlow = async (intent: DownloadIntent, root: ActiveRoot, origin: DownloadOrigin) => {
    if (downloadPickerOpen.current) return;
    const scope = scopeRef.current;
    if (!scope || !selectionIsCurrent(scope, root, origin)) {
      options.setStatus("Downloads require the active live file workspace.");
      return;
    }
    downloadPickerOpen.current = true;
    const chosen = await chooseDownloadDestination(intent)
      .catch((error) => {
        options.setStatus(`Could not open the save panel: ${String(error)}`);
        return undefined;
      })
      .finally(() => { downloadPickerOpen.current = false; });
    if (!chosen) return;
    if (!selectionIsCurrent(scope, root, origin)) {
      options.setStatus("Download cancelled because the active host or workspace changed.");
      return;
    }
    await startDownload(scope, {
      path: intent.path,
      kind: intent.kind,
      destination: chosen.destination,
      collision: chosen.panelConfirmed ? "overwrite" : "fail",
    }, root, origin);
  };

  return { mutateFile, startDownloadFlow };
}

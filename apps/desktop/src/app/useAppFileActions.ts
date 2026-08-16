import { useRef, type Dispatch, type SetStateAction } from "react";
import { keyForTransferConnection } from "../features/files/api";
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

/** Owns filesystem mutation, native save-panel serialization, and transfer publication. */
export function useAppFileActions(options: AppFileActionsOptions) {
  const downloadPickerOpen = useRef(false);

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

  const startDownload = async (request: DownloadRequest, root: ActiveRoot) => {
    if (!options.scope) throw new Error("Downloads require a live file host.");
    try {
      const transfer = await options.client.startDownload(options.scope, root, request);
      options.recordTransfer(transfer);
      const banner = `Download ${transfer.state}: ${request.path}`;
      options.setActiveDownloadStatus({ id: transfer.id, path: request.path, banner });
      options.setStatus(banner);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      options.recordTransfer({
        id: crypto.randomUUID(),
        scopeKey: keyForTransferConnection(options.scope),
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

  const startDownloadFlow = async (intent: DownloadIntent, root: ActiveRoot) => {
    if (downloadPickerOpen.current) return;
    downloadPickerOpen.current = true;
    const chosen = await chooseDownloadDestination(intent)
      .catch((error) => {
        options.setStatus(`Could not open the save panel: ${String(error)}`);
        return undefined;
      })
      .finally(() => { downloadPickerOpen.current = false; });
    if (!chosen) return;
    await startDownload({
      path: intent.path,
      kind: intent.kind,
      destination: chosen.destination,
      collision: chosen.panelConfirmed ? "overwrite" : "fail",
    }, root);
  };

  return { mutateFile, startDownloadFlow };
}

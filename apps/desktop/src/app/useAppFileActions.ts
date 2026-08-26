import { useRef, type Dispatch, type SetStateAction } from "react";
import { recordIncident } from "../diagnostics/incidents";
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

function classifyDownloadAdmissionError(message: string) {
  if (message.startsWith("destination parent is unavailable or unsafe")) {
    return "parentUnavailableOrUnsafe";
  }
  if (message.startsWith("destination is not writable")) return "destinationNotWritable";
  if (message === "destination already exists") return "destinationAlreadyExists";
  if (message.includes("already reserved by another transfer")) return "destinationAlreadyReserved";
  if (message.startsWith("download destination") || message.startsWith("destination basename")) {
    return "invalidDestination";
  }
  return "other";
}

/**
 * Explains a selection rejection without recording host IDs, paths, or root
 * capability tokens. The booleans are enough to distinguish a startup gap,
 * a host switch, a pane switch, and a still-valid tab rooted somewhere other
 * than the live Explorer.
 */
export function downloadSelectionEvidence(
  capturedScope: FileWorkspaceScope | undefined,
  requestedRoot: ActiveRoot,
  currentScope: FileWorkspaceScope | undefined,
  currentRoot: ActiveRoot | undefined,
) {
  return {
    capturedScopePresent: Boolean(capturedScope),
    currentScopePresent: Boolean(currentScope),
    currentRootPresent: Boolean(currentRoot),
    scopeKeyMatch: Boolean(capturedScope && currentScope
      && keyForScope(capturedScope) === keyForScope(currentScope)),
    clientMatch: Boolean(capturedScope && currentScope && capturedScope.clientId === currentScope.clientId),
    serverMatch: Boolean(capturedScope && currentScope && capturedScope.serverIdentity === currentScope.serverIdentity),
    epochMatch: Boolean(capturedScope && currentScope && capturedScope.terminalEpoch === currentScope.terminalEpoch),
    sessionMatch: Boolean(capturedScope && currentScope && capturedScope.sessionId === currentScope.sessionId),
    scopePaneMatch: Boolean(capturedScope && currentScope && capturedScope.paneId === currentScope.paneId),
    rootMatch: sameRoot(currentRoot, requestedRoot),
    rootTokenMatch: Boolean(currentRoot && currentRoot.token === requestedRoot.token),
    rootPathMatch: Boolean(currentRoot && currentRoot.path === requestedRoot.path),
    rootPaneMatch: Boolean(currentRoot && currentRoot.paneId === requestedRoot.paneId),
  };
}

/** Owns filesystem mutation, native save-panel serialization, and transfer publication. */
export function useAppFileActions(options: AppFileActionsOptions) {
  const downloadPickerOpen = useRef(false);
  const scopeRef = useRef(options.scope);
  const rootRef = useRef(options.root);
  scopeRef.current = options.scope;
  rootRef.current = options.root;

  const selectionIsCurrent = (scope: FileWorkspaceScope, root: ActiveRoot) => {
    const currentScope = scopeRef.current;
    return Boolean(currentScope
      && keyForScope(currentScope) === keyForScope(scope)
      && sameRoot(rootRef.current, root));
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
    if (!selectionIsCurrent(scope, root)) {
      recordIncident("download.workspaceRejected", {
        attemptId: request.diagnosticAttemptId, origin, stage: "preStart",
        ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
      });
      options.setStatus("Download cancelled because the active host or workspace changed.");
      return;
    }
    try {
      const transfer = await options.client.startDownload(scope, root, request);
      if (!selectionIsCurrent(scope, root)) {
        await options.client.cancelTransfer(scope, transfer.id).catch(() => undefined);
        recordIncident("download.workspaceRejected", {
          attemptId: request.diagnosticAttemptId, origin, stage: "postAdmission",
          ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
        });
        options.setStatus("Download cancelled because the active host or workspace changed.");
        return;
      }
      options.recordTransfer(transfer);
      const banner = `Download ${transfer.state}: ${request.path}`;
      options.setActiveDownloadStatus({ id: transfer.id, path: request.path, banner });
      options.setStatus(banner);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (!selectionIsCurrent(scope, root)) {
        recordIncident("download.workspaceRejected", {
          attemptId: request.diagnosticAttemptId, origin, stage: "admissionErrorAfterSelectionChanged",
          ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
        });
        options.setStatus("Download cancelled because the active host or workspace changed.");
        return;
      }
      recordIncident("download.admissionFailed", {
        attemptId: request.diagnosticAttemptId, origin,
        errorClass: classifyDownloadAdmissionError(message),
        ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
      });
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
    const attemptId = crypto.randomUUID();
    const scope = scopeRef.current;
    recordIncident("download.requested", {
      attemptId, origin, intentKind: intent.kind,
      ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
    });
    if (!scope || !selectionIsCurrent(scope, root)) {
      recordIncident("download.workspaceRejected", {
        attemptId, origin, stage: "prePicker",
        ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
      });
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
    recordIncident("download.destinationChosen", {
      attemptId, origin, intentKind: intent.kind, panelConfirmed: chosen.panelConfirmed,
    });
    if (!selectionIsCurrent(scope, root)) {
      recordIncident("download.workspaceRejected", {
        attemptId, origin, stage: "postPicker",
        ...downloadSelectionEvidence(scope, root, scopeRef.current, rootRef.current),
      });
      options.setStatus("Download cancelled because the active host or workspace changed.");
      return;
    }
    await startDownload(scope, {
      path: intent.path,
      kind: intent.kind,
      destination: chosen.destination,
      collision: chosen.panelConfirmed ? "overwrite" : "fail",
      diagnosticAttemptId: attemptId,
    }, root, origin);
  };

  return { mutateFile, startDownloadFlow };
}

import type { FileWorkspaceClient, FileWorkspaceScope } from "../features/files/types";
import type { useWorkspaceFiles } from "../features/files/useWorkspaceFiles";
import { ExplorerTree } from "../features/files/ExplorerTree";
import type { DownloadIntent } from "../features/files/downloadFlow";
import { keyForScope } from "../features/files/api";
import type { FileEntry, FileMutation } from "../features/files/types";
import type { GitWorkspaceClient } from "../features/git/types";
import type { GitDiffTarget, GitStatusEntry } from "../features/git/types";
import type { WorkspaceGitState } from "../features/git/useWorkspaceGit";
import { GitSidebar } from "../features/git/GitSidebar";
import { RightPanel } from "../features/shell/RightPanel";
import type { ShellState } from "../features/shell/types";

interface AppRightPanelProps {
  canMutate: boolean;
  fileClient: FileWorkspaceClient;
  fileScope?: FileWorkspaceScope;
  ignoredPaths?: ReadonlySet<string>;
  onDownload: (intent: DownloadIntent) => Promise<void>;
  onGitDiff: (entry: GitStatusEntry, target: GitDiffTarget) => void;
  onMessage: (message: string) => void;
  onMutate: (mutation: FileMutation) => Promise<void>;
  onOpenFile: (entry: FileEntry, options: { preview: boolean }) => void;
  onSurface: (surface: ShellState["panelSurface"]) => void;
  surface: ShellState["panelSurface"];
  workspaceFiles: ReturnType<typeof useWorkspaceFiles>;
  workspaceGit: WorkspaceGitState;
  gitClient: GitWorkspaceClient;
}

/** Explorer and Git rail wiring, kept outside the root application coordinator. */
export function AppRightPanel(props: AppRightPanelProps) {
  return <RightPanel
    files={<ExplorerTree
      disabled={!props.canMutate}
      error={props.workspaceFiles.error}
      expanded={props.workspaceFiles.expanded}
      ignoredPaths={props.ignoredPaths}
      listings={props.workspaceFiles.listings}
      loading={props.workspaceFiles.loading}
      requestedReads={props.workspaceFiles.requestedReads}
      onCancelTransfer={async (id) => {
        if (props.fileScope) await props.fileClient.cancelTransfer(props.fileScope, id);
      }}
      onDownload={props.onDownload}
      onLoadMore={props.workspaceFiles.loadMore}
      onMutate={props.onMutate}
      onOpen={props.onOpenFile}
      onRefresh={props.workspaceFiles.refresh}
      onToggle={props.workspaceFiles.toggleDirectory}
      root={props.workspaceFiles.root}
      scopeIdentity={props.fileScope ? keyForScope(props.fileScope) : "disconnected"}
      transfers={props.workspaceFiles.transfers}
    />}
    git={<GitSidebar
      client={props.gitClient}
      disabled={!props.canMutate}
      error={props.workspaceGit.error}
      loading={props.workspaceGit.loading}
      onMessage={props.onMessage}
      onOpenDiff={props.onGitDiff}
      onRefresh={() => void props.workspaceGit.refresh()}
      onStatus={props.workspaceGit.accept}
      root={props.workspaceFiles.root}
      scope={props.fileScope}
      status={props.workspaceGit.status}
    />}
    onSurface={props.onSurface}
    surface={props.surface}
  />;
}

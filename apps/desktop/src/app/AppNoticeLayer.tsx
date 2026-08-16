import type { ReactNode } from "react";
import type { DownloadCompletion } from "../features/files/downloadStatus";
import { DownloadActions } from "../features/files/DownloadActions";
import type { StatusNotice } from "../features/shell/statusNotice";
import { SurfaceError } from "../ui/SurfaceError";
import type { TerminalTransferClient } from "../features/terminal/terminalTransfers";
import type { TerminalTransferRegistry } from "../features/terminal/terminalTransferRegistry";
import { TerminalTransferHistory } from "../features/terminal/TerminalTransferSurface";

interface AppNoticeLayerProps {
  agentDialog: ReactNode;
  agentSetupDialog: ReactNode;
  appStateRecovery?: string;
  completedDownload?: DownloadCompletion & { noticeId?: number };
  notice?: StatusNotice;
  onClearDownload: () => void;
  onDismissNotice: () => void;
  onDownloadResult: (error?: string) => void;
  onOfferDiscard: () => void;
  onOfferRestore: () => void;
  onProfileReset: () => void;
  onStateReset: () => void;
  onTransferError: (error: unknown) => void;
  profileRecovery?: { error: string; preservedPath: string } | null;
  recoveryOffer?: { count: number };
  terminalTransferClient: TerminalTransferClient;
  terminalTransferRegistry: TerminalTransferRegistry;
}

/** Fixed-position status, recovery, and transfer surfaces outside workspace layout. */
export function AppNoticeLayer(props: AppNoticeLayerProps) {
  return <>
    {props.notice && <div className={props.notice.severity === "problem" ? "toast toast-problem" : "toast"} role={props.notice.severity === "problem" ? "alert" : "status"}>
      {props.notice.severity === "problem"
        ? <SurfaceError className="toast-body" detail={props.notice.message} role="none" />
        : <span>{props.notice.message}</span>}
      <div>
        {props.completedDownload?.noticeId === props.notice.id && <DownloadActions
          destination={props.completedDownload.destination}
          onResult={props.onDownloadResult}
        />}
        <button aria-label="Dismiss" onClick={() => { props.onDismissNotice(); props.onClearDownload(); }} type="button">Dismiss</button>
      </div>
    </div>}
    {props.profileRecovery && <div className="toast" role="alert"><strong>Saved host profiles were recovered</strong><span>{props.profileRecovery.error} The original was preserved at {props.profileRecovery.preservedPath}.</span><button onClick={props.onProfileReset} type="button">Confirm recovered defaults…</button></div>}
    {props.appStateRecovery && <div className="toast" role="alert"><strong>Saved shell state is write-frozen</strong><span>{props.appStateRecovery}</span><button onClick={props.onStateReset} type="button">Reset saved shell state…</button></div>}
    {props.recoveryOffer && <div className="toast" role="status"><strong>App tabs found from the replaced tmux server</strong><span>{props.recoveryOffer.count} tab{props.recoveryOffer.count === 1 ? "" : "s"} can be rebound by unique workspace name. Terminal and pane identities are never reused.</span><div><button onClick={props.onOfferRestore} type="button">Restore app tabs</button><button onClick={props.onOfferDiscard} type="button">Discard old tabs…</button></div></div>}
    <TerminalTransferHistory client={props.terminalTransferClient} onError={props.onTransferError} registry={props.terminalTransferRegistry} />
    {props.agentDialog}
    {props.agentSetupDialog}
  </>;
}

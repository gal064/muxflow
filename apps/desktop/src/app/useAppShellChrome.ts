import { useEffect, useRef, useState } from "react";
import type { DownloadCompletion } from "../features/files/downloadStatus";
import { noticeDismissDelay, noticeForStatus, type StatusNotice } from "../features/shell/statusNotice";

/** Below this the sidebar overlays the terminal instead of taking space. */
const COMPACT_VIEWPORT_QUERY = "(max-width: 880px)";

/** Owns viewport-derived rails and the shell's status-to-notice lifecycle. */
/** `sequence` changes on every `setStatus`, so a repeated message re-notifies. */
export function useAppShellChrome(status: string, sequence = 0) {
  const [compactViewport, setCompactViewport] = useState(
    () => window.matchMedia?.(COMPACT_VIEWPORT_QUERY).matches ?? false,
  );
  const [windowWidth, setWindowWidth] = useState(() => window.innerWidth || 1280);
  const [completedDownload, setCompletedDownload] = useState<DownloadCompletion & { noticeId?: number }>();
  const [notice, setNotice] = useState<StatusNotice>();
  const noticeSequence = useRef(0);

  useEffect(() => {
    const next = noticeForStatus(status, (noticeSequence.current += 1));
    // Routine chatter is the shell narrating itself — "Live" after every
    // snapshot, "Topology changed; reconciling…" after every mutation — and it
    // is not an instruction to take down what is already on screen. Treating it
    // as one meant any message a mutation produced was erased by the mutation's
    // own follow-up statuses, usually within the same frame: a bulk close's
    // receipt and, worse, a refusal that was never meant to auto-dismiss.
    if (next) setNotice(next);
    // The download's Reveal/Open actions hang off the notice by id, so the two
    // have to be retired by the same rule. Cleared on chatter while the notice
    // it belongs to stayed up, a completed download became a toast naming a
    // file with nothing to do with it.
    setCompletedDownload((current) => {
      if (!next) return current;
      return current && current.message.trim() === next.message ? { ...current, noticeId: next.id } : undefined;
    });
  }, [sequence, status]);

  // Keyed on the notice rather than on the status that produced it: a notice
  // that outlives a routine status has to keep its own clock, or holding it
  // through the chatter above would hold it forever.
  useEffect(() => {
    if (!notice) return;
    const delay = noticeDismissDelay(notice);
    if (delay === undefined) return;
    const timer = window.setTimeout(
      () => setNotice((current) => current?.id === notice.id ? undefined : current),
      delay,
    );
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    if (!window.matchMedia) return;
    const query = window.matchMedia(COMPACT_VIEWPORT_QUERY);
    setCompactViewport(query.matches);
    const handleChange = (event: MediaQueryListEvent) => setCompactViewport(event.matches);
    query.addEventListener("change", handleChange);
    return () => query.removeEventListener("change", handleChange);
  }, []);

  useEffect(() => {
    const onResize = () => setWindowWidth(window.innerWidth || 1280);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  return {
    compactViewport,
    completedDownload,
    notice,
    setCompletedDownload,
    setNotice,
    windowWidth,
  };
}

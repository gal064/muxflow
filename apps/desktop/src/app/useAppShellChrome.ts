import { useEffect, useRef, useState } from "react";
import type { DownloadCompletion } from "../features/files/downloadStatus";
import { noticeDismissDelay, noticeForStatus, type StatusNotice } from "../features/shell/statusNotice";

/** Below this the sidebar overlays the terminal instead of taking space. */
const COMPACT_VIEWPORT_QUERY = "(max-width: 880px)";

/** Owns viewport-derived rails and the shell's status-to-notice lifecycle. */
export function useAppShellChrome(status: string) {
  const [compactViewport, setCompactViewport] = useState(
    () => window.matchMedia?.(COMPACT_VIEWPORT_QUERY).matches ?? false,
  );
  const [windowWidth, setWindowWidth] = useState(() => window.innerWidth || 1280);
  const [completedDownload, setCompletedDownload] = useState<DownloadCompletion & { noticeId?: number }>();
  const [notice, setNotice] = useState<StatusNotice>();
  const noticeSequence = useRef(0);

  useEffect(() => {
    const next = noticeForStatus(status, (noticeSequence.current += 1));
    setNotice(next);
    setCompletedDownload((current) => current && next && current.message.trim() === next.message
      ? { ...current, noticeId: next.id }
      : undefined);
    if (!next) return;
    const delay = noticeDismissDelay(next);
    if (delay === undefined) return;
    const timer = window.setTimeout(
      () => setNotice((current) => current?.id === next.id ? undefined : current),
      delay,
    );
    return () => window.clearTimeout(timer);
  }, [status]);

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

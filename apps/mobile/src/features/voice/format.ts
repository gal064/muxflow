// Copy helpers for the Voice screen (design.md §9.11).

/** `m:ss` for a position or duration; `0:00` when unknown. */
export function formatClock(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** `640 MB`-style sizes for the consent copy and the progress line. */
export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  if (bytes >= 1024 * 1024) return `${Math.round(bytes / (1024 * 1024))} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

const PHASE_LABELS: Record<string, string> = {
  installing_runtime: "Installing the speech runtime…",
  downloading: "Downloading the speech model…",
  extracting: "Unpacking the model…",
  verifying: "Verifying the model…",
  ready: "Ready",
  failed: "Setup failed",
};

export function provisionPhaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? "Setting up voice…";
}

/** 0..1 when the total is known, else undefined (an indeterminate bar). */
export function provisionFraction(transferred: number, total: number): number | undefined {
  if (total <= 0) return undefined;
  return Math.min(1, Math.max(0, transferred / total));
}

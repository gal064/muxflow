import { invoke } from "@tauri-apps/api/core";

/**
 * Always-on incident journal — the renderer half of `incidents.rs`.
 *
 * Every connection rebuild, link degradation, and pane watchdog episode calls
 * this with the reason it knows at the moment of deciding. The record lands in
 * `incidents.jsonl` under the app's log directory, so the next "the amber bar
 * keeps flashing" investigation starts from the actual trigger instead of a
 * theory. This is not the perf probe: that one is an opt-in measurement
 * campaign, while incidents are rare and must be recorded in every build.
 *
 * Fire-and-forget by contract. A journal that cannot be written (tests, a
 * full disk, a torn-down webview) must never become an error the app reacts
 * to, so every failure path here ends in silence.
 */

/** Groups one app launch's records without needing a session boundary line. */
const launchId = typeof crypto === "undefined" ? "unknown" : crypto.randomUUID().slice(0, 8);

/**
 * The record's own frame. A detail that happens to carry one of these — an
 * event `kind`, say — used to spread over the label and leave the line
 * unattributable, so the frame is written first and the detail cannot reach it.
 */
const reservedKeys = new Set(["t", "launch", "kind"]);

export function recordIncident(kind: string, detail?: Record<string, unknown>): void {
  try {
    const record: Record<string, unknown> = { t: new Date().toISOString(), launch: launchId, kind };
    for (const [key, value] of Object.entries(detail ?? {})) {
      if (!reservedKeys.has(key)) record[key] = value;
    }
    const line = JSON.stringify(record);
    void invoke("record_incident", { line }).catch(() => undefined);
  } catch {
    // Serialization failed or no Tauri runtime; the journal misses one line.
  }
}

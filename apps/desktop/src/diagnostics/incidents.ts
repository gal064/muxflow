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

export function recordIncident(kind: string, detail?: Record<string, unknown>): void {
  try {
    const line = JSON.stringify({ t: new Date().toISOString(), launch: launchId, kind, ...detail });
    void invoke("record_incident", { line }).catch(() => undefined);
  } catch {
    // Serialization failed or no Tauri runtime; the journal misses one line.
  }
}

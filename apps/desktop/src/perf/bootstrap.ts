import { invoke } from "@tauri-apps/api/core";
import { enablePerfProbe, flushPerfProbe } from "./probe";

/**
 * Enables Phase 12 latency instrumentation only when the desktop process was
 * launched with `ADE_PERF_LOG` pointing at an absolute path. The host owns that
 * decision; the page cannot turn instrumentation on for itself.
 */
export async function bootstrapPerfProbe(): Promise<boolean> {
  let enabled = false;
  try {
    enabled = await invoke<boolean>("perf_log_enabled");
  } catch {
    return false;
  }
  if (!enabled) return false;
  enablePerfProbe((lines) => invoke("append_perf_log", { lines }));
  window.addEventListener("beforeunload", () => {
    void flushPerfProbe();
  });
  return true;
}

import { invoke } from "@tauri-apps/api/core";
import type { AutomationDefinition, AutomationListing, SchedulePreview } from "./types";

type Command =
  | { action: "list" }
  | { action: "save"; definition: AutomationDefinition; baseGeneration: string }
  | { action: "delete"; id: string; baseGeneration: string }
  | { action: "runNow"; id: string }
  | { action: "previewSchedule"; schedule: string };

const request = <T>(clientId: string, command: Command) =>
  invoke<T>("automation_request", { clientId, command });

/** One host's automations, through that host's live bridge. */
export const automationsApi = {
  list: (clientId: string) => request<AutomationListing>(clientId, { action: "list" }),
  save: (clientId: string, definition: AutomationDefinition, baseGeneration: string) =>
    request<AutomationListing>(clientId, { action: "save", definition, baseGeneration }),
  remove: (clientId: string, id: string, baseGeneration: string) =>
    request<AutomationListing>(clientId, { action: "delete", id, baseGeneration }),
  runNow: (clientId: string, id: string) => request<AutomationListing>(clientId, { action: "runNow", id }),
  previewSchedule: (clientId: string, schedule: string) =>
    request<SchedulePreview>(clientId, { action: "previewSchedule", schedule }),
};

/** The host refused a save because the file changed since it was listed. */
export function isStaleGeneration(error: unknown): boolean {
  return String(error).includes("stale_generation");
}

/** The host's refusal, without its machine-readable code. */
export function refusalText(error: unknown): string {
  return String(error).replace(/^[a-z_]+: /, "");
}

const listeners = new Set<(hostProfileId: string) => void>();

/** Called by the connection controller when a host says its automations changed. */
export function publishAutomationChanged(hostProfileId: string): void {
  for (const listener of listeners) listener(hostProfileId);
}

export function onAutomationChanged(listener: (hostProfileId: string) => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

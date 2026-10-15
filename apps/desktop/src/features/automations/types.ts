/** One entry of the host's automations.json (docs/automations.md). */
export interface AutomationDefinition {
  id: string;
  name: string;
  enabled: boolean;
  /** Cron, in the host's local time. */
  schedule: string;
  cwd: string;
  /** `claude`, `codex` or `custom`; anything else is an entry to fix. */
  agent: string;
  /** Typed as is for `custom`; the presets ignore it. */
  command: string;
  prompt: string;
  keepTabs: number;
  maxRuntimeMinutes: number;
}

export type AutomationRunStatus =
  | "launched" | "working" | "needs_attention" | "done" | "closed" | "failed" | "skipped";

export interface AutomationRun {
  number: number;
  /** The host-local minute it was due, `YYYY-MM-DDTHH:MM`; empty for Run now. */
  scheduledFor: string;
  startedAt: number;
  finishedAt: number;
  status: AutomationRunStatus | string;
  sessionId: string;
  windowId: string;
  paneId: string;
  serverIdentity: string;
  lastMessage: string;
  error: string;
}

export interface AutomationSummary {
  definition: AutomationDefinition;
  /** Why the entry does not run; empty when it is valid. */
  definitionError: string;
  /** The next host-local minute it runs, or empty. */
  nextRun: string;
  /** Newest first. */
  recentRuns: AutomationRun[];
  /** What is typed into a run's tab. */
  commandLine: string;
}

export interface AutomationListing {
  automations: AutomationSummary[];
  /** Why automations.json cannot be read; the last set that loaded still runs. */
  fileError: string;
  /** Names the version of the file listed; saves are refused over a newer one. */
  generation: string;
  stateDir: string;
}

export interface SchedulePreview {
  nextRuns: string[];
  scheduleError: string;
}

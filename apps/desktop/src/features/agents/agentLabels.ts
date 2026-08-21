import type { AgentAdapterDescriptor, AgentAdapterId, AgentRecord } from "./types";

const UUID_LIKE = /^(?:[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}|[0-9a-f]{24,})$/i;
const GENERIC_TAB_NAME = /^(?:agent|codex|claude(?:-code)?)$/i;

/** The live tmux tab name, unless it is only a generic or machine identifier. */
export function agentSessionLabel(agent: AgentRecord, adapters: readonly AgentAdapterDescriptor[]): string {
  const tabName = agent.windowName.trim();
  if (tabName && !UUID_LIKE.test(tabName) && !GENERIC_TAB_NAME.test(tabName)) return tabName;
  const assignedName = agent.displayName.trim();
  if (assignedName && !UUID_LIKE.test(assignedName)) return assignedName;
  return adapters.find((adapter) => adapter.id === agent.adapterId)?.displayName
    || adapterFallback(agent.adapterId);
}

function adapterFallback(adapterId: AgentAdapterId): string {
  if (adapterId === "codex") return "Codex";
  if (adapterId === "claude-code" || adapterId === "claude") return "Claude";
  return "Agent";
}

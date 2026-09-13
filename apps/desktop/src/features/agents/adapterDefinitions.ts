import type { AgentAdapterId } from "./types";

export function canonicalAdapterId(adapterId: string | undefined): AgentAdapterId {
  if (!adapterId) throw new Error("Agent adapter identity is missing.");
  return adapterId;
}

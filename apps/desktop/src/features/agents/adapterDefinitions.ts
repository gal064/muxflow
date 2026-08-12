import type { AgentAdapterId } from "./types";

/** The only compatibility boundary for legacy protobuf enum spellings. */
const LEGACY_ADAPTER_IDS: Readonly<Record<string, AgentAdapterId>> = {
  codex: "codex",
  claude: "claude-code",
  claude_code: "claude-code",
  claudeCode: "claude-code",
};

export function canonicalAdapterId(adapterId: string | undefined, legacyAdapter?: string): AgentAdapterId {
  const candidate = adapterId && adapterId !== "unspecified" ? adapterId : legacyAdapter;
  if (!candidate || candidate === "unspecified") throw new Error("Agent adapter identity is missing.");
  return LEGACY_ADAPTER_IDS[candidate] ?? candidate;
}

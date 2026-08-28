import type { AgentRecord } from "./types";
import { agentGeneration } from "./generation";

type AgentFixtureOverrides = Partial<Omit<AgentRecord, "lifecycleGeneration" | "attentionGeneration" | "seenGeneration">> & {
  lifecycleGeneration?: string | number;
  attentionGeneration?: string | number;
  seenGeneration?: string | number;
};

export const agent = (overrides: AgentFixtureOverrides = {}): AgentRecord => ({
  id: "agent-1", adapterId: "codex", nativeSessionId: "native-1", displayName: "Codex one",
  hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", sessionName: "work",
  windowId: "@1", windowName: "agent", paneId: "%1", lifecycle: "working",
  lifecycleGeneration: agentGeneration(overrides.lifecycleGeneration ?? 3),
  attentionGeneration: agentGeneration(overrides.attentionGeneration ?? 0),
  seenGeneration: agentGeneration(overrides.seenGeneration ?? 0),
  updatedAt: 100, lifecycleChangedAt: 100, detectedManually: false, present: true,
  ...Object.fromEntries(Object.entries(overrides).filter(([key]) => !["lifecycleGeneration", "attentionGeneration", "seenGeneration"].includes(key))),
});

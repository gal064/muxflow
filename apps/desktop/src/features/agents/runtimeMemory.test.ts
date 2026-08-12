import { describe, expect, it } from "vitest";
import { agentGeneration } from "./generation";
import { AgentRuntimeMemory, MAX_AGENT_RUNTIME_SCOPES, MAX_PROCESSED_ATTENTION, MAX_RECONNECT_RECORDS_PER_SCOPE } from "./runtimeMemory";
import { agent } from "./testFixtures";

describe("AgentRuntimeMemory", () => {
  it("bounds processed generations, host scopes, and reconnect records", () => {
    const memory = new AgentRuntimeMemory();
    for (let index = 0; index < MAX_PROCESSED_ATTENTION + 20; index += 1) memory.markProcessed(`record-${index}`, agentGeneration(index));
    expect(memory.processedSize).toBe(MAX_PROCESSED_ATTENTION);
    expect(memory.processedGeneration("record-0")).toBeUndefined();
    for (let scope = 0; scope < MAX_AGENT_RUNTIME_SCOPES + 2; scope += 1) {
      const records = Object.fromEntries(Array.from({ length: MAX_RECONNECT_RECORDS_PER_SCOPE + 5 }, (_, index) => {
        const record = agent({ id: `${scope}-${index}`, updatedAt: index });
        return [record.id, record];
      }));
      memory.commitScope(`host-${scope}`, agentGeneration(scope), records);
    }
    expect(memory.scopeSize).toBe(MAX_AGENT_RUNTIME_SCOPES);
    expect(memory.scope("host-0")).toBeUndefined();
    expect(Object.keys(memory.scope(`host-${MAX_AGENT_RUNTIME_SCOPES + 1}`)!.records)).toHaveLength(MAX_RECONNECT_RECORDS_PER_SCOPE);
  });
});

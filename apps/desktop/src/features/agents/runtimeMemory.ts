import type { AgentGeneration } from "./generation";
import type { AgentRecord } from "./types";

export const MAX_AGENT_RUNTIME_SCOPES = 16;
export const MAX_PROCESSED_ATTENTION = 4096;
export const MAX_RECONNECT_RECORDS_PER_SCOPE = 2048;

interface ScopeMemory {
  watermark: AgentGeneration;
  records: Readonly<Record<string, AgentRecord>>;
}

/** Bounded LRU memory used only for reconnect notification reconciliation. */
export class AgentRuntimeMemory {
  readonly #processedAttention = new Map<string, AgentGeneration>();
  readonly #scopes = new Map<string, ScopeMemory>();

  processedGeneration(recordKey: string): AgentGeneration | undefined {
    return this.#processedAttention.get(recordKey);
  }

  markProcessed(recordKey: string, generation: AgentGeneration): void {
    this.#processedAttention.delete(recordKey);
    this.#processedAttention.set(recordKey, generation);
    trimOldest(this.#processedAttention, MAX_PROCESSED_ATTENTION);
  }

  scope(scopeKey: string): ScopeMemory | undefined {
    const memory = this.#scopes.get(scopeKey);
    if (!memory) return undefined;
    this.#scopes.delete(scopeKey);
    this.#scopes.set(scopeKey, memory);
    return memory;
  }

  commitScope(scopeKey: string, watermark: AgentGeneration, records: Readonly<Record<string, AgentRecord>>): void {
    this.#scopes.delete(scopeKey);
    this.#scopes.set(scopeKey, { watermark, records: boundedRecords(records) });
    while (this.#scopes.size > MAX_AGENT_RUNTIME_SCOPES) {
      const oldest = this.#scopes.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.#scopes.delete(oldest);
      const prefix = `${oldest}\0`;
      for (const key of this.#processedAttention.keys()) {
        if (key.startsWith(prefix)) this.#processedAttention.delete(key);
      }
    }
  }

  get processedSize(): number { return this.#processedAttention.size; }
  get scopeSize(): number { return this.#scopes.size; }
}

function boundedRecords(records: Readonly<Record<string, AgentRecord>>): Readonly<Record<string, AgentRecord>> {
  const values = Object.values(records);
  if (values.length <= MAX_RECONNECT_RECORDS_PER_SCOPE) return records;
  return Object.fromEntries(values
    .sort((left, right) => right.updatedAt - left.updatedAt)
    .slice(0, MAX_RECONNECT_RECORDS_PER_SCOPE)
    .map((record) => [record.id, record]));
}

function trimOldest<Key, Value>(map: Map<Key, Value>, maximum: number): void {
  while (map.size > maximum) {
    const oldest = map.keys().next().value as Key | undefined;
    if (oldest === undefined) break;
    map.delete(oldest);
  }
}

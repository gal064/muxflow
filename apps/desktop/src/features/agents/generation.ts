declare const generationBrand: unique symbol;

/** Canonical unsigned decimal. Kept as text so the full protobuf u64 range is lossless. */
export type AgentGeneration = string & { readonly [generationBrand]: true };

export const zeroGeneration = "0" as AgentGeneration;

export function agentGeneration(value: string | number | bigint, label = "agent generation"): AgentGeneration {
  const text = typeof value === "string" ? value : value.toString();
  if (!/^[0-9]+$/.test(text)) throw new Error(`${label} must be an unsigned decimal string.`);
  if (typeof value === "number" && (!Number.isSafeInteger(value) || value < 0)) {
    throw new Error(`${label} must arrive as a decimal string outside JavaScript's safe range.`);
  }
  return text.replace(/^0+(?=\d)/, "") as AgentGeneration;
}

export function compareAgentGenerations(left: AgentGeneration, right: AgentGeneration): -1 | 0 | 1 {
  if (left.length !== right.length) return left.length < right.length ? -1 : 1;
  if (left === right) return 0;
  return left < right ? -1 : 1;
}

export function generationIsAfter(left: AgentGeneration, right: AgentGeneration): boolean {
  return compareAgentGenerations(left, right) > 0;
}

export function generationAtLeast(left: AgentGeneration, right: AgentGeneration): boolean {
  return compareAgentGenerations(left, right) >= 0;
}

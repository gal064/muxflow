import { describe, expect, it } from "vitest";
import { agentGeneration, compareAgentGenerations, generationAtLeast, generationIsAfter } from "./generation";

describe("lossless agent generations", () => {
  it("orders the complete protobuf u64 range without Number coercion", () => {
    const safe = agentGeneration("9007199254740991");
    const unsafe = agentGeneration("9007199254740993");
    const maximum = agentGeneration("18446744073709551615");
    expect(generationIsAfter(unsafe, safe)).toBe(true);
    expect(generationIsAfter(maximum, unsafe)).toBe(true);
    expect(generationAtLeast(maximum, maximum)).toBe(true);
    expect(compareAgentGenerations(safe, maximum)).toBe(-1);
  });

  it("canonicalizes leading zeros and rejects unsafe numeric input", () => {
    expect(agentGeneration("00042")).toBe("42");
    expect(() => agentGeneration(Number.MAX_SAFE_INTEGER + 1)).toThrow("decimal string");
    expect(() => agentGeneration("-1")).toThrow("unsigned decimal");
  });
});

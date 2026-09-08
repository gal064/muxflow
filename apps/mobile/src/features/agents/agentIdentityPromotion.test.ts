import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { AgentEventSchema, AgentRecordSchema } from "../../protocol/gen/envelope_pb";
import type { Agent } from "../../store/sessionStore";
import { deriveAgentIdentityPromotion } from "./agentIdentityPromotion";

function agent(id: string, adapterId = "codex", paneId = "%7", nativeSessionId = id === "native" ? "native-session" : ""): Agent {
  return {
    id,
    adapterId,
    nativeSessionId,
    displayName: "Agent",
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 2n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 2,
    lifecycleChangedAtMs: 2,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId: "$1", sessionNameFallback: "", windowId: "@1", windowNameFallback: "", paneId, paneIndexFallback: 0 },
  };
}

describe("deriveAgentIdentityPromotion", () => {
  it("accepts only ids retired by the same insertion on the same adapter and non-empty pane", () => {
    const old = agent("manual");
    const next = agent("native");
    const event = create(AgentEventSchema, {
      agent: create(AgentRecordSchema, { agentId: "native" }),
      retiredAgentIds: ["manual"],
    });
    expect(deriveAgentIdentityPromotion({ manual: old }, event, next)).toEqual({ retiredAgentIds: ["manual"], agent: next });
    expect(deriveAgentIdentityPromotion({ manual: old }, create(AgentEventSchema, { agent: event.agent }), next)).toBeUndefined();
    expect(deriveAgentIdentityPromotion({ manual: agent("manual", "claude-code") }, event, next)).toBeUndefined();
    expect(deriveAgentIdentityPromotion({ manual: agent("manual", "codex", "%8") }, event, next)).toBeUndefined();
    expect(deriveAgentIdentityPromotion({ manual: agent("manual", "codex", "") }, event, next)).toBeUndefined();
    expect(deriveAgentIdentityPromotion({ manual: old }, event, agent("native", "codex", "%7", ""))).toBeUndefined();
    expect(deriveAgentIdentityPromotion({ manual: agent("manual", "codex", "%7", "old-native") }, event, next)).toBeUndefined();
  });

  it("returns every qualifying retired candidate without choosing by array order", () => {
    const next = agent("native");
    const event = create(AgentEventSchema, {
      agent: create(AgentRecordSchema, { agentId: "native" }),
      retiredAgentIds: ["manual-b", "manual-a", "manual-b"],
    });
    expect(deriveAgentIdentityPromotion({ "manual-a": agent("manual-a"), "manual-b": agent("manual-b") }, event, next)?.retiredAgentIds)
      .toEqual(["manual-b", "manual-a"]);
  });
});

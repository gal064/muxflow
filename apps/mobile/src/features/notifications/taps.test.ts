import { describe, expect, it } from "vitest";

import { toRouteParam } from "../../navigation/routeParams";
import { decodePayload, encodePayload, type TapTarget } from "./payload";
import { createTapMarkSeen, terminalRoute } from "./taps";

const target: TapTarget = { agentId: "a1", paneId: "%12", sessionId: "$3", attentionGeneration: 7n, serverIdentity: "tmux:/s:1" };

describe("notification payloads", () => {
  it("round-trips §13 step 7's data through the bridge's JSON", () => {
    const encoded = encodePayload({ agentId: "a1", paneId: "%12", sessionId: "$3", attentionGeneration: 7n }, "tmux:/s:1");
    expect(encoded).toEqual({ agentId: "a1", paneId: "%12", sessionId: "$3", attentionGeneration: "7", serverIdentity: "tmux:/s:1" });
    expect(decodePayload(encoded)).toEqual(target);
  });

  it("survives a generation past Number.MAX_SAFE_INTEGER", () => {
    const generation = 9007199254740993n;
    expect(decodePayload(encodePayload({ ...target, attentionGeneration: generation }, "tmux:/s:1")))
      .toMatchObject({ attentionGeneration: generation });
  });

  it("drops anything that is not one of ours", () => {
    // The foreground service's ongoing notification (§6.3) carries no payload.
    expect(decodePayload(undefined)).toBeUndefined();
    expect(decodePayload({})).toBeUndefined();
    expect(decodePayload({ agentId: "a1" })).toBeUndefined();
    expect(decodePayload({ agentId: "a1", paneId: "%12", attentionGeneration: "nope" })).toBeUndefined();
    expect(decodePayload({ agentId: "a1", paneId: "", sessionId: "$3", attentionGeneration: "7" })).toBeUndefined();
  });

  it("accepts a re-serialised numeric generation and a missing session", () => {
    expect(decodePayload({ agentId: "a1", paneId: "%12", attentionGeneration: 7 }))
      .toEqual({ ...target, sessionId: "", serverIdentity: "" });
  });
});

describe("tap routing (§13)", () => {
  it("routes a tap payload to the agent's terminal", () => {
    expect(terminalRoute(target)).toEqual({
      pathname: "/terminal/[paneId]",
      params: { paneId: toRouteParam("%12"), sessionId: toRouteParam("$3") },
    });
  });
});

describe("mark-seen from a tap", () => {
  it("sends straight away when there is a connection", () => {
    const sent: TapTarget[] = [];
    const markSeen = createTapMarkSeen((t) => {
      sent.push(t);
      return true;
    });
    markSeen.request(target);
    expect(sent).toEqual([target]);
    expect(markSeen.pending()).toBe(0);
  });

  it("holds a cold-start tap until a connection is up, then sends it once", () => {
    const sent: TapTarget[] = [];
    let connected = false;
    const markSeen = createTapMarkSeen((t) => {
      if (!connected) return false;
      sent.push(t);
      return true;
    });
    markSeen.request(target);
    expect(sent).toEqual([]);
    expect(markSeen.pending()).toBe(1);

    markSeen.flush();
    expect(sent).toEqual([]);

    connected = true;
    markSeen.flush();
    expect(sent).toEqual([target]);
    markSeen.flush();
    expect(sent).toEqual([target]);
    expect(markSeen.pending()).toBe(0);
  });

  it("holds a tap aimed at a host that is not the connected one", () => {
    // Pane and session ids are tmux ids and collide across hosts, so a held
    // acknowledgement must not be flushed at whichever host answers next.
    const sent: TapTarget[] = [];
    let identity = "tmux:/other:9";
    const markSeen = createTapMarkSeen((t) => {
      if (t.serverIdentity !== identity) return false;
      sent.push(t);
      return true;
    });
    markSeen.request(target);
    markSeen.flush();
    expect(sent).toEqual([]);
    identity = "tmux:/s:1";
    markSeen.flush();
    expect(sent).toEqual([target]);
  });

  it("keeps only the newest generation per agent while holding", () => {
    const sent: TapTarget[] = [];
    let connected = false;
    const markSeen = createTapMarkSeen((t) => {
      if (!connected) return false;
      sent.push(t);
      return true;
    });
    markSeen.request(target);
    markSeen.request({ ...target, attentionGeneration: 9n });
    markSeen.request({ ...target, agentId: "a2" });
    expect(markSeen.pending()).toBe(2);
    connected = true;
    markSeen.flush();
    expect(sent).toEqual([{ ...target, attentionGeneration: 9n }, { ...target, agentId: "a2" }]);
  });
});

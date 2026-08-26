import { describe, expect, it } from "vitest";

import {
  backoffSeconds,
  connectingStripText,
  connectionStateLabel,
  reconnectingStripText,
} from "./connectionLabels";

describe("connection strip copy (§9 global chrome, §12)", () => {
  it("counts the §7.2 backoff down, and at 0 says exactly what §9 says", () => {
    expect(reconnectingStripText(17)).toBe("Connection lost. Reconnecting in 17s…");
    expect(reconnectingStripText(0)).toBe("Reconnecting…");
    expect(reconnectingStripText(0, "Couldn't reach devbox:22.")).toBe("Reconnecting…");
  });

  it("says why when the §12 row knows", () => {
    expect(reconnectingStripText(4, "Couldn't reach devbox:22.")).toBe(
      "Couldn't reach devbox:22. Reconnecting in 4s…",
    );
    expect(reconnectingStripText(4, "   ")).toBe("Connection lost. Reconnecting in 4s…");
    expect(reconnectingStripText(4, undefined)).toBe("Connection lost. Reconnecting in 4s…");
  });

  it("matches §7.2's schedule: 1, 2, 4, … capped at 30 s", () => {
    expect([1, 2, 3, 4, 5, 6, 7].map(backoffSeconds)).toEqual([1, 2, 4, 8, 16, 30, 30]);
  });

  it("names the connecting host", () => {
    expect(connectingStripText("Docker")).toBe("Connecting to Docker…");
  });

  it("gives every state a human name (§9.8)", () => {
    expect(connectionStateLabel("connected")).toBe("Connected");
    expect(connectionStateLabel("awaitingHostKeyTrust")).toBe("Waiting for host key trust");
    expect(connectionStateLabel("incompatible")).toBe("Incompatible helper");
  });
});

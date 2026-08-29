import { describe, expect, it } from "vitest";
import { userFacingBridgeFailure } from "./bridgeFailureText";

describe("userFacingBridgeFailure", () => {
  // Pinned against the exact shape connection/bridge.rs produces: a wording
  // change there would otherwise leak the internal reason into a notice.
  it("strips the supervisor's teardown annotation and nothing else", () => {
    expect(userFacingBridgeFailure(
      "frame I/O failed: failed to fill whole buffer (torn down locally: a request went unanswered past its deadline)",
    )).toBe("frame I/O failed: failed to fill whole buffer");
    expect(userFacingBridgeFailure("host bridge closed")).toBe("host bridge closed");
  });
});

import { describe, expect, it } from "vitest";
import {
  isTransientRevealError,
  REVEAL_RETRY_LIMIT,
  revealFailureAction,
  TRANSIENT_REVEAL_ERRORS,
} from "./revealRetry";

describe("isTransientRevealError", () => {
  // The host returns these as bare strings, so the whole rejection is the
  // message the native command wrote. Matching each in the exact shape
  // `connection.rs` produces is what keeps a rename from going unnoticed.
  it("recognizes every native phrase that means the transport is not ready", () => {
    expect(isTransientRevealError("terminal visibility checkpoint belongs to a stale connection epoch")).toBe(true);
    expect(isTransientRevealError(new Error("host bridge is disconnected"))).toBe(true);
    expect(isTransientRevealError("terminal client is no longer attached")).toBe(true);
    expect(TRANSIENT_REVEAL_ERRORS).toHaveLength(3);
  });

  it("treats anything else as a real failure", () => {
    expect(isTransientRevealError(new Error("visibility conflict"))).toBe(false);
    expect(isTransientRevealError("pane %1 is unknown to this session")).toBe(false);
    expect(isTransientRevealError(undefined)).toBe(false);
  });
});

describe("revealFailureAction", () => {
  it("ignores a failure whose attempt has been superseded, transient or not", () => {
    const superseded = { current: false, retriesUsed: 0 };
    expect(revealFailureAction({ ...superseded, error: "host bridge is disconnected" })).toBe("ignore");
    // The one that matters: a slow-failing reveal must not mark a pane the
    // newer reveal already made healthy as degraded.
    expect(revealFailureAction({ ...superseded, error: new Error("visibility conflict") })).toBe("ignore");
  });

  it("retries a transient failure while the attempt has budget left", () => {
    const error = "terminal visibility checkpoint belongs to a stale connection epoch";
    expect(revealFailureAction({ error, current: true, retriesUsed: 0 })).toBe("retry");
    expect(revealFailureAction({ error, current: true, retriesUsed: REVEAL_RETRY_LIMIT - 1 })).toBe("retry");
    // Exhausted: the watchdog takes over rather than this looping forever.
    expect(revealFailureAction({ error, current: true, retriesUsed: REVEAL_RETRY_LIMIT })).toBe("degrade");
  });

  it("degrades a real conflict on its first failure", () => {
    expect(revealFailureAction({ error: new Error("visibility conflict"), current: true, retriesUsed: 0 }))
      .toBe("degrade");
  });
});

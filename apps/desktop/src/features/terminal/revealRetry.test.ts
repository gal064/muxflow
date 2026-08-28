import { describe, expect, it } from "vitest";
import {
  isStaleEpochRevealError,
  isTransientRevealError,
  REVEAL_RETRY_LIMIT,
  revealFailureAction,
  STALE_REVEAL_EPOCH_CODE,
  TRANSIENT_REVEAL_ERRORS,
} from "./revealRetry";

/** Exactly what `terminal_visibility_request` returns for a superseded epoch. */
const STALE_EPOCH = `${STALE_REVEAL_EPOCH_CODE}: terminal visibility checkpoint belongs to a stale connection epoch`;

describe("isTransientRevealError", () => {
  // The host returns these as bare strings, so the whole rejection is the
  // message the native command wrote. Matching each in the exact shape
  // `connection.rs` produces is what keeps a rename from going unnoticed.
  it("recognizes every native refusal that means the transport is not ready", () => {
    expect(isTransientRevealError(new Error("host bridge is disconnected"))).toBe(true);
    expect(isTransientRevealError("terminal client is no longer attached")).toBe(true);
    expect(isTransientRevealError("connection_unavailable: host connection is disconnected or reconciling")).toBe(true);
    expect(TRANSIENT_REVEAL_ERRORS).toHaveLength(3);
  });

  it("treats anything else as a real failure", () => {
    expect(isTransientRevealError(new Error("visibility conflict"))).toBe(false);
    expect(isTransientRevealError("pane %1 is unknown to this session")).toBe(false);
    expect(isTransientRevealError(undefined)).toBe(false);
  });

  // The whole point of the split: a request the host will refuse identically
  // forever must not be counted as "the transport is coming up".
  it("no longer counts a stale checkpoint epoch as transient", () => {
    expect(isTransientRevealError(STALE_EPOCH)).toBe(false);
    expect(isStaleEpochRevealError(STALE_EPOCH)).toBe(true);
    expect(isStaleEpochRevealError(new Error(STALE_EPOCH))).toBe(true);
    expect(isStaleEpochRevealError("host bridge is disconnected")).toBe(false);
  });
});

describe("revealFailureAction", () => {
  it("ignores a failure whose attempt has been superseded, transient or not", () => {
    const superseded = { current: false, retriesUsed: 0 };
    expect(revealFailureAction({ ...superseded, error: "host bridge is disconnected" })).toBe("ignore");
    expect(revealFailureAction({ ...superseded, error: STALE_EPOCH })).toBe("ignore");
    // The one that matters: a slow-failing reveal must not mark a pane the
    // newer reveal already made healthy as degraded.
    expect(revealFailureAction({ ...superseded, error: new Error("visibility conflict") })).toBe("ignore");
  });

  it("retries a transient failure while the attempt has budget left", () => {
    const error = "host bridge is disconnected";
    expect(revealFailureAction({ error, current: true, retriesUsed: 0 })).toBe("retry");
    expect(revealFailureAction({ error, current: true, retriesUsed: REVEAL_RETRY_LIMIT - 1 })).toBe("retry");
    // Exhausted: the watchdog takes over rather than this looping forever.
    expect(revealFailureAction({ error, current: true, retriesUsed: REVEAL_RETRY_LIMIT })).toBe("degrade");
  });

  it("rebuilds a stale-epoch refusal on its first failure, whatever the budget", () => {
    // No attempt of this request can succeed, so the budget is irrelevant: the
    // answer is the same on the first failure and on the last.
    expect(revealFailureAction({ error: STALE_EPOCH, current: true, retriesUsed: 0 })).toBe("rebuild");
    expect(revealFailureAction({ error: new Error(STALE_EPOCH), current: true, retriesUsed: 3 })).toBe("rebuild");
  });

  it("degrades a real conflict on its first failure", () => {
    expect(revealFailureAction({ error: new Error("visibility conflict"), current: true, retriesUsed: 0 }))
      .toBe("degrade");
  });

  // The refusal every reconnect produces: the reveal the epoch frame triggers
  // reaches the transport one round trip before its `ready` gate opens. It
  // used to be classified as a conflict, which reseeded through the same gate
  // and showed the user an error toast for a pane that healed 2s later.
  it("retries the not-ready refusal instead of degrading on it", () => {
    const error = "connection_unavailable: host connection is disconnected or reconciling";
    expect(revealFailureAction({ error, current: true, retriesUsed: 0 })).toBe("retry");
    expect(revealFailureAction({ error, current: true, retriesUsed: REVEAL_RETRY_LIMIT })).toBe("degrade");
  });
});

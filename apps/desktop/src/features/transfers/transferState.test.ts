import { describe, expect, it } from "vitest";
import {
  canCancelTransfer,
  isTerminalTransferState,
  mergeCanonicalTransfer,
  transferOutcomeFromWire,
  transferStateFromWire,
  transferStateLabel,
  validateTransferStateOutcome,
  type TransferState,
  type CanonicalTransferSnapshot,
} from "./transferState";

const states: TransferState[] = [
  "queued", "preflighting", "running", "verifying", "completed", "cancelled", "failed",
];
const snapshot = (value: CanonicalTransferSnapshot): CanonicalTransferSnapshot => value;

describe("canonical transfer state", () => {
  it("maps every backend state explicitly and rejects future unknown states", () => {
    expect(["queued", "preflighting", "running", "verifying", "completed", "cancelled", "failed"].map(transferStateFromWire)).toEqual([
      "queued", "preflighting", "running", "verifying", "completed", "cancelled", "failed",
    ]);
    expect(() => transferStateFromWire("future-state")).toThrow("Unknown transfer state");
    expect(() => transferStateFromWire("progress")).toThrow("Unknown transfer state");
  });

  it("gives every canonical state an explicit label, terminal flag, and cancellation policy", () => {
    expect(states.map(transferStateLabel)).toHaveLength(states.length);
    expect(states.filter(isTerminalTransferState)).toEqual(["completed", "cancelled", "failed"]);
    expect(states.filter(canCancelTransfer)).toEqual(["queued", "preflighting", "running"]);
  });

  it("keeps publication outcome distinct from lifecycle state", () => {
    expect(transferOutcomeFromWire(undefined, "running")).toBeUndefined();
    expect(() => transferOutcomeFromWire(undefined, "completed")).toThrow("omitted its outcome");
    expect(transferOutcomeFromWire("notPublished", "failed")).toBe("notPublished");
    expect(transferOutcomeFromWire("unknown", "failed")).toBe("unknown");
    expect(() => transferOutcomeFromWire("maybe", "failed")).toThrow("Unknown transfer outcome");
    expect(() => validateTransferStateOutcome("running", undefined, undefined)).not.toThrow();
    expect(() => validateTransferStateOutcome("failed", "unknown", "outcomeUnknown")).not.toThrow();
    expect(() => validateTransferStateOutcome("failed", "unknown", "timeout")).not.toThrow();
    expect(() => validateTransferStateOutcome("failed", "unknown", "staleScope")).not.toThrow();
    expect(() => validateTransferStateOutcome("failed", "notPublished", undefined)).toThrow("failure kind");
    expect(() => validateTransferStateOutcome("failed", "unknown", "cleanup")).toThrow("requires");
    expect(() => validateTransferStateOutcome("cancelled", "notPublished", undefined)).not.toThrow();
    expect(() => validateTransferStateOutcome("cancelled", "notPublished", "transfer")).toThrow("Only a failed transfer");
    expect(() => validateTransferStateOutcome("cancelled", "published", undefined)).toThrow("notPublished");
  });

  it("merges cleanup and lifecycle monotonically across late cancellation/no-op frames", () => {
    const failed: CanonicalTransferSnapshot = { state: "failed", outcome: "notPublished", failureKind: "cleanup", cleanupStatus: "failed", cleanupError: "rollback failed", error: "upload failed" };
    expect(mergeCanonicalTransfer(failed, snapshot({
      state: "cancelled", outcome: "notPublished", cleanupStatus: "removed", error: "cancel no-op",
    }))).toEqual(failed);
    expect(mergeCanonicalTransfer(
      snapshot({ state: "failed", outcome: "notPublished", failureKind: "transfer", cleanupStatus: "retained", cleanupError: "quarantined" }),
      snapshot({ state: "failed", outcome: "notPublished", failureKind: "transfer", cleanupStatus: "failed", cleanupError: "rollback unavailable" }),
    )).toMatchObject({ cleanupStatus: "failed", cleanupError: "rollback unavailable" });
    expect(mergeCanonicalTransfer(
      snapshot({ state: "failed", outcome: "notPublished", failureKind: "cleanup", cleanupStatus: "failed", cleanupError: "first failure" }),
      snapshot({ state: "failed", outcome: "notPublished", failureKind: "cleanup", cleanupStatus: "failed", cleanupError: "later failure" }),
    )).toMatchObject({ cleanupError: "first failure" });
    expect(mergeCanonicalTransfer(
      snapshot({ state: "cancelled", outcome: "notPublished", cleanupStatus: "connectionClosed", cleanupError: "connection lost during cleanup" }),
      snapshot({ state: "cancelled", outcome: "notPublished", cleanupStatus: "removed" }),
    )).toMatchObject({ cleanupStatus: "connectionClosed", cleanupError: "connection lost during cleanup" });
  });
});

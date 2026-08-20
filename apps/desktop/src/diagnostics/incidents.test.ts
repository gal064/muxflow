import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { recordIncident } from "./incidents";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));

describe("recordIncident", () => {
  beforeEach(() => vi.mocked(invoke).mockClear());

  it("records one JSON line carrying the kind, the detail, and a launch id", () => {
    recordIncident("reconnect.flowStall", { paneId: "%7" });
    const line = vi.mocked(invoke).mock.calls[0]?.[1] as { line: string };
    const record = JSON.parse(line.line);
    expect(record.kind).toBe("reconnect.flowStall");
    expect(record.paneId).toBe("%7");
    expect(record.launch).toHaveLength(8);
    expect(Number.isNaN(Date.parse(record.t))).toBe(false);
  });

  it("stays silent when the journal cannot be written", () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("no runtime"));
    expect(() => recordIncident("link.degraded", { phase: "resyncing" })).not.toThrow();
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";

import { LOG_BYTE_CAPACITY, LOG_EVENT_CAPACITY, createLogStore, log, logStore, logText, redactSecrets } from "./log";

describe("the memory-only diagnostic flight recorder", () => {
  afterEach(() => {
    logStore.getState().clear();
    vi.restoreAllMocks();
  });

  it("uses the agreed process-memory bounds", () => {
    expect(LOG_EVENT_CAPACITY).toBe(1_000);
    expect(LOG_BYTE_CAPACITY).toBe(256 * 1024);
  });

  it("keeps the newest events by count and preserves timestamp/sequence ordering", () => {
    let now = Date.parse("2026-09-05T12:00:00.000Z");
    const store = createLogStore({ eventCapacity: 3, now: () => now++ });
    for (const line of ["a", "b", "c", "d"]) store.getState().append(line);
    expect(store.getState().lines).toEqual([
      "2026-09-05T12:00:00.001Z #000002 [muxflow] b",
      "2026-09-05T12:00:00.002Z #000003 [muxflow] c",
      "2026-09-05T12:00:00.003Z #000004 [muxflow] d",
    ]);
    expect(logText(store.getState())).toBe(store.getState().lines.join("\n"));
  });

  it("overwrites oldest events when the UTF-8 byte bound is reached", () => {
    const store = createLogStore({ eventCapacity: 10, byteCapacity: 118, now: () => 0 });
    store.getState().append("one");
    store.getState().append("two");
    store.getState().append("three");
    expect(store.getState().lines.map((line) => line.split(" ").at(-1))).toEqual(["two", "three"]);
    expect(new TextEncoder().encode(logText(store.getState())).byteLength).toBe(store.getState().bytes);
    expect(store.getState().bytes).toBeLessThanOrEqual(118);
  });

  it("counts multi-byte text without depending on a runtime TextEncoder", () => {
    const store = createLogStore({ byteCapacity: 1_000, now: () => 0 });
    store.getState().append("ascii é 🚀");
    expect(store.getState().bytes).toBe(new TextEncoder().encode(logText(store.getState())).byteLength);
  });

  it("redacts key bodies, credentials, tokens, private-key blocks, and embedded passwords", () => {
    const key = "AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleEx";
    const dirty = `publicKey=${key} token=abc.def password='hello world' OPENAI_API_KEY=sk-openai GITHUB_TOKEN=ghp_token AWS_SECRET_ACCESS_KEY=aws-secret {"access_token":"json-secret","service.client-secret":"client-value"} Bearer eyJhbGciOiJI ssh-ed25519 ${key} https://me:hunter2@example.test -----BEGIN OPENSSH PRIVATE KEY-----\nbody\n-----END OPENSSH PRIVATE KEY-----`;
    const clean = redactSecrets(dirty);
    expect(clean).not.toContain(key);
    expect(clean).not.toContain("abc.def");
    expect(clean).not.toContain("hello world");
    expect(clean).not.toContain("eyJhbGciOiJI");
    expect(clean).not.toContain("hunter2");
    expect(clean).not.toContain("json-secret");
    expect(clean).not.toContain("sk-openai");
    expect(clean).not.toContain("ghp_token");
    expect(clean).not.toContain("aws-secret");
    expect(clean).not.toContain("client-value");
    expect(clean).not.toContain("PRIVATE KEY-----");
    expect(clean).toContain("ssh-ed25519 …");
  });

  it("keeps host-key fingerprints readable", () => {
    const line = "hostKey algorithm=ssh-ed25519 fingerprint=SHA256:5Y1kSQQpQeF0N0h5W2QMOB1x5EJ0gk2z9m7lF3hQ0aY";
    expect(redactSecrets(line)).toBe(line);
  });

  it("makes the shared logger immediately visible to the copyable store and redacts console output too", () => {
    const consoleLog = vi.spyOn(console, "log").mockImplementation(() => undefined);
    log("voice agent=a recorder.error token=do-not-copy");
    const copied = logText(undefined, ["diagnostics copied token=also-secret"]);
    expect(copied).toContain("voice agent=a recorder.error token=<redacted>");
    expect(copied).toContain("diagnostics copied token=<redacted>");
    expect(copied).not.toContain("do-not-copy");
    expect(copied).not.toContain("also-secret");
    expect(consoleLog).toHaveBeenCalledWith(logStore.getState().lines[0]);
  });
});

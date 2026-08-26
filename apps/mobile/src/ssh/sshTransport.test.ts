import { describe, expect, it, vi } from "vitest";

import { base64ToBytes, bytesToBase64 } from "./base64";
import { BRIDGE_COMMAND, openSshTransport, type HostKeyPrompt } from "./sshTransport";
import { TransportDialError, type Transport, type TransportClose } from "../protocol/Transport";
import type { MuxflowSsh, SshCloseReason, SshEvent } from "./MuxflowSsh";

const target = { host: "10.0.2.2", port: 22222, user: "ade" };
const hostAddress = { host: "10.0.2.2", port: 22222 };

/** A stand-in for the native module: records calls, emits events on demand. */
function fakeSsh() {
  const listeners = new Set<(event: SshEvent) => void>();
  const connects: { connectionId: string; command: string; trusted: string | null }[] = [];
  const writes: { connectionId: string; base64: string }[] = [];
  const closes: string[] = [];
  const trusts: { connectionId: string; fingerprint: string }[] = [];
  const ssh: MuxflowSsh = {
    generateKeyPair: vi.fn(async () => ({ publicKeyOpenSsh: "ssh-ed25519 AAAA muxflow-mobile" })),
    getPublicKey: vi.fn(async () => null),
    deleteKeyPair: vi.fn(async () => undefined),
    connect: vi.fn(async (connectionId, _target, command, trusted) => {
      connects.push({ connectionId, command, trusted });
    }),
    trustHostKey: vi.fn(async (connectionId, fingerprint) => {
      trusts.push({ connectionId, fingerprint });
    }),
    write: vi.fn(async (connectionId, base64) => {
      writes.push({ connectionId, base64 });
    }),
    close: vi.fn(async (connectionId) => {
      closes.push(connectionId);
    }),
    startForegroundService: vi.fn(async () => undefined),
    stopForegroundService: vi.fn(async () => undefined),
    addListener: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  const emit = (event: SshEvent) => {
    for (const listener of [...listeners]) listener(event);
  };
  return { ssh, emit, connects, writes, closes, trusts, listenerCount: () => listeners.size };
}

async function connected(overrides: Partial<Parameters<typeof openSshTransport>[0]> = {}) {
  const fake = fakeSsh();
  const connectionId = overrides.connectionId ?? "host.control.1";
  const pending = openSshTransport({
    connectionId,
    target,
    trustedHostKeyFingerprint: "SHA256:pinned",
    ssh: fake.ssh,
    hostAddress,
    ...overrides,
  });
  fake.emit({ type: "connected", connectionId });
  return { fake, transport: await pending, connectionId };
}

describe("openSshTransport", () => {
  it("runs the desktop's bridge command and resolves once it is running", async () => {
    const { fake, transport } = await connected();
    expect(fake.connects).toEqual([
      { connectionId: "host.control.1", command: BRIDGE_COMMAND, trusted: "SHA256:pinned" },
    ]);
    expect(BRIDGE_COMMAND).toBe("$HOME/.local/bin/muxflow-host bridge --stdio");
    expect(transport).toBeTruthy();
  });

  it("moves bytes in both directions", async () => {
    const { fake, transport, connectionId } = await connected();
    const seen: Uint8Array[] = [];
    transport.onData((chunk) => seen.push(chunk));
    fake.emit({ type: "data", connectionId, base64: bytesToBase64(Uint8Array.of(0, 1, 2, 250)) });
    expect(seen).toEqual([Uint8Array.of(0, 1, 2, 250)]);

    transport.write(Uint8Array.of(7, 8));
    expect(fake.writes).toEqual([{ connectionId, base64: bytesToBase64(Uint8Array.of(7, 8)) }]);
    expect(base64ToBytes(fake.writes[0]?.base64 as string)).toEqual(Uint8Array.of(7, 8));
  });

  it("replays stdout that arrived before the client attached", async () => {
    const { fake, transport, connectionId } = await connected();
    fake.emit({ type: "data", connectionId, base64: bytesToBase64(Uint8Array.of(1)) });
    const seen: Uint8Array[] = [];
    transport.onData((chunk) => seen.push(chunk));
    expect(seen).toEqual([Uint8Array.of(1)]);
  });

  it("maps every close reason to its §12 row", async () => {
    const rows: { reason: SshCloseReason; exitCode: number | null; expected: TransportClose }[] = [
      {
        reason: "connectFailed",
        exitCode: null,
        expected: { reason: "connectFailed", message: "Couldn't reach 10.0.2.2:22222." },
      },
      {
        reason: "authFailed",
        exitCode: null,
        expected: {
          reason: "authFailed",
          message:
            "10.0.2.2 rejected this phone's SSH key. Add the key under Your SSH key to ~/.ssh/authorized_keys on the host.",
        },
      },
      {
        reason: "hostKeyMismatch",
        exitCode: null,
        expected: {
          reason: "hostKeyMismatch",
          message:
            "The host key for 10.0.2.2 has changed. This can mean the machine was reinstalled — or that something is intercepting the connection. Connection refused.",
        },
      },
      {
        reason: "exited",
        exitCode: 127,
        expected: {
          reason: "exited",
          exitCode: 127,
          message:
            "muxflow-host isn't installed on this host. Install it from the Muxflow desktop app (Settings → Connection).",
        },
      },
      {
        reason: "exited",
        exitCode: 2,
        expected: { reason: "exited", exitCode: 2, message: "The helper exited (code 2)." },
      },
      {
        reason: "networkLost",
        exitCode: null,
        expected: { reason: "networkLost", message: "Connection lost." },
      },
      {
        reason: "closedByClient",
        exitCode: null,
        expected: { reason: "localClose", message: "Disconnected." },
      },
    ];
    for (const row of rows) {
      const { fake, transport, connectionId } = await connected();
      const closes: TransportClose[] = [];
      transport.onClosed((close) => closes.push(close));
      fake.emit({ type: "closed", connectionId, exitCode: row.exitCode, reason: row.reason });
      expect(closes).toEqual([row.expected]);
    }
  });

  it("reports the helper's first stderr line for an ordinary non-zero exit", async () => {
    const { fake, transport, connectionId } = await connected();
    const closes: TransportClose[] = [];
    transport.onClosed((close) => closes.push(close));
    fake.emit({ type: "stderr", connectionId, text: "muxflow-host: unknown flag\nusage: ...\n" });
    fake.emit({ type: "closed", connectionId, exitCode: 64, reason: "exited" });
    expect(closes[0]?.message).toBe("muxflow-host: unknown flag");
  });

  it("rejects the dial when the channel closes before it is running", async () => {
    const fake = fakeSsh();
    const pending = openSshTransport({
      connectionId: "c",
      target,
      trustedHostKeyFingerprint: null,
      ssh: fake.ssh,
      hostAddress,
    });
    fake.emit({ type: "closed", connectionId: "c", exitCode: null, reason: "connectFailed" });
    await expect(pending).rejects.toBeInstanceOf(TransportDialError);
    await pending.catch((error: TransportDialError) => {
      expect(error.close).toEqual({ reason: "connectFailed", message: "Couldn't reach 10.0.2.2:22222." });
    });
  });

  it("trusts a host key only when the dialog answers yes", async () => {
    const fake = fakeSsh();
    const prompts: HostKeyPrompt[] = [];
    const pending = openSshTransport({
      connectionId: "c",
      target,
      trustedHostKeyFingerprint: null,
      ssh: fake.ssh,
      hostAddress,
      onHostKey: async (prompt) => {
        prompts.push(prompt);
        return true;
      },
    });
    fake.emit({ type: "hostKey", connectionId: "c", algorithm: "ssh-ed25519", fingerprintSha256: "SHA256:new" });
    await vi.waitFor(() => expect(fake.trusts).toEqual([{ connectionId: "c", fingerprint: "SHA256:new" }]));
    expect(prompts[0]).toMatchObject({ algorithm: "ssh-ed25519", fingerprintSha256: "SHA256:new", target });
    fake.emit({ type: "connected", connectionId: "c" });
    await expect(pending).resolves.toBeTruthy();
  });

  it("refusing the dialog closes the channel and reports hostKeyNotTrusted", async () => {
    const fake = fakeSsh();
    const closes: TransportClose[] = [];
    const pending = openSshTransport({
      connectionId: "c",
      target,
      trustedHostKeyFingerprint: null,
      ssh: fake.ssh,
      hostAddress,
      onHostKey: async () => false,
      onClose: (close) => closes.push(close),
    });
    fake.emit({ type: "hostKey", connectionId: "c", algorithm: "ssh-ed25519", fingerprintSha256: "SHA256:new" });
    await vi.waitFor(() => expect(fake.closes).toEqual(["c"]));
    expect(fake.trusts).toEqual([]);
    // The native module calls a refused key `closedByClient`; only the JS side
    // knows the user refused it, so it names the §12 reason.
    fake.emit({ type: "closed", connectionId: "c", exitCode: null, reason: "closedByClient" });
    await expect(pending).rejects.toBeInstanceOf(TransportDialError);
    expect(closes).toEqual([
      {
        reason: "hostKeyNotTrusted",
        message: "The host key for 10.0.2.2 wasn't trusted. Connection refused.",
      },
    ]);
  });

  it("takes the dialog down when the dial itself fails under it", async () => {
    const fake = fakeSsh();
    let cancelled = 0;
    fake.ssh.connect = vi.fn(async () => {
      throw new Error("no route to host");
    });
    const pending = openSshTransport({
      connectionId: "c",
      target,
      trustedHostKeyFingerprint: null,
      ssh: fake.ssh,
      hostAddress,
      // Never answers: the dialog is still on screen when the dial gives up.
      onHostKey: () => new Promise<boolean>(() => undefined),
      onHostKeyCancelled: () => {
        cancelled += 1;
      },
    });
    fake.emit({ type: "hostKey", connectionId: "c", algorithm: "ssh-ed25519", fingerprintSha256: "SHA256:new" });
    await expect(pending).rejects.toBeInstanceOf(TransportDialError);
    // Without this the store stays busy and refuses every later prompt.
    expect(cancelled).toBe(1);
  });

  it("never trusts a key when no dialog is wired", async () => {
    const fake = fakeSsh();
    void openSshTransport({
      connectionId: "c",
      target,
      trustedHostKeyFingerprint: null,
      ssh: fake.ssh,
      hostAddress,
    }).catch(() => undefined);
    fake.emit({ type: "hostKey", connectionId: "c", algorithm: "ssh-ed25519", fingerprintSha256: "SHA256:new" });
    await vi.waitFor(() => expect(fake.closes).toEqual(["c"]));
    expect(fake.trusts).toEqual([]);
  });

  it("gives each lane its own connection id and closes both (§11.1)", async () => {
    const fake = fakeSsh();
    const lanes: Transport[] = [];
    for (const connectionId of ["host.control.1", "host.bulk.2"]) {
      const pending = openSshTransport({
        connectionId,
        target,
        trustedHostKeyFingerprint: "SHA256:pinned",
        ssh: fake.ssh,
        hostAddress,
      });
      fake.emit({ type: "connected", connectionId });
      lanes.push(await pending);
    }
    expect(fake.connects.map((call) => call.connectionId)).toEqual(["host.control.1", "host.bulk.2"]);

    // Bytes on one lane never reach the other.
    const control: Uint8Array[] = [];
    const bulk: Uint8Array[] = [];
    lanes[0]?.onData((chunk) => control.push(chunk));
    lanes[1]?.onData((chunk) => bulk.push(chunk));
    fake.emit({ type: "data", connectionId: "host.bulk.2", base64: bytesToBase64(Uint8Array.of(9)) });
    expect(control).toEqual([]);
    expect(bulk).toEqual([Uint8Array.of(9)]);

    for (const lane of lanes) lane.close();
    expect(fake.closes).toEqual(["host.control.1", "host.bulk.2"]);
  });

  it("stops listening once the channel is gone", async () => {
    const { fake, transport, connectionId } = await connected();
    transport.onClosed(() => undefined);
    expect(fake.listenerCount()).toBe(1);
    fake.emit({ type: "closed", connectionId, exitCode: 0, reason: "exited" });
    expect(fake.listenerCount()).toBe(0);
  });
});

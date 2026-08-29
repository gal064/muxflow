import { describe, expect, it } from "vitest";

import { describeConnectionFailure, describeTransportClose, firstStderrLine } from "./errorMatrix";
import type { TransportClose } from "../../protocol/Transport";

const host = { host: "devbox", port: 22 };

describe("the §12 error matrix", () => {
  it("maps every row to its copy, surface and retry policy", () => {
    const rows: [TransportClose, ReturnType<typeof describeTransportClose>][] = [
      [
        { reason: "connectFailed" },
        { presentation: "strip", retryable: true, message: "Couldn't reach devbox:22." },
      ],
      [
        { reason: "authFailed" },
        {
          presentation: "fullScreen",
          retryable: false,
          action: { kind: "sshKey", label: "Your SSH key" },
          message:
            "devbox rejected this phone's SSH login. Over Tailscale SSH, check the tailnet's SSH policy; otherwise add the key under Your SSH key to ~/.ssh/authorized_keys on the host.",
        },
      ],
      [
        { reason: "exited", exitCode: 127 },
        {
          presentation: "fullScreen",
          retryable: false,
          message:
            "muxflow-host isn't installed on this host. Install it from the Muxflow desktop app (Settings → Connection).",
        },
      ],
      [
        { reason: "exited", exitCode: 1, message: "bash: line 1: muxflow-hosts: command not found" },
        {
          presentation: "strip",
          retryable: true,
          message: "bash: line 1: muxflow-hosts: command not found",
        },
      ],
      [
        { reason: "networkLost" },
        { presentation: "strip", retryable: true, message: "Connection lost." },
      ],
      [
        { reason: "hostKeyMismatch" },
        {
          presentation: "fullScreen",
          retryable: false,
          action: { kind: "forgetHostKey", label: "Forget host key" },
          message:
            "The host key for devbox has changed. This can mean the machine was reinstalled — or that something is intercepting the connection. Connection refused.",
        },
      ],
      [
        { reason: "hostKeyNotTrusted" },
        {
          presentation: "strip",
          retryable: false,
          message: "The host key for devbox wasn't trusted. Connection refused.",
        },
      ],
      [
        { reason: "localClose" },
        { presentation: "strip", retryable: false, message: "Disconnected." },
      ],
    ];
    for (const [close, expected] of rows) {
      expect(describeTransportClose(close, host), close.reason).toEqual(expected);
    }
  });

  it("names the host generically when there is none", () => {
    expect(describeTransportClose({ reason: "connectFailed" }).message).toBe("Couldn't reach the host.");
    expect(describeTransportClose({ reason: "authFailed" }).message).toMatch(/^The host rejected/);
  });

  it("falls back to an exit code when the helper printed nothing", () => {
    expect(describeTransportClose({ reason: "exited", exitCode: 2 }, host).message).toBe(
      "The helper exited (code 2).",
    );
    expect(describeTransportClose({ reason: "exited" }, host).message).toBe("The helper exited.");
  });

  it("takes the first non-empty stderr line", () => {
    expect(firstStderrLine("\n\n  boom  \nsecond\n")).toBe("boom");
    expect(firstStderrLine("")).toBeUndefined();
    expect(firstStderrLine(undefined)).toBeUndefined();
  });

  it("shows an incompatible helper full-screen with Reconnect now (§7.3)", () => {
    expect(
      describeConnectionFailure({
        state: "incompatible",
        message: "The Muxflow helper on this host is missing: files. Update it from the Muxflow desktop app.",
        host,
      }),
    ).toEqual({
      presentation: "fullScreen",
      retryable: false,
      action: { kind: "reconnect", label: "Reconnect now" },
      message: "The Muxflow helper on this host is missing: files. Update it from the Muxflow desktop app.",
    });
  });

  it("diagnoses a failed connection from its last close", () => {
    expect(
      describeConnectionFailure({ state: "failed", message: "whatever", close: { reason: "authFailed" }, host }),
    ).toMatchObject({ presentation: "fullScreen", action: { kind: "sshKey" } });
  });

  it("keeps the state machine's message when the failure closed the transport itself", () => {
    // `HostConnection.fail()` records why, then closes the transport, which
    // reports `localClose`; the close says nothing the message does not.
    expect(
      describeConnectionFailure({
        state: "failed",
        message: "host did not return ServerHello",
        close: { reason: "localClose", message: "Disconnected." },
        host,
      }),
    ).toEqual({
      presentation: "strip",
      retryable: false,
      message: "host did not return ServerHello",
    });
  });

  it("falls back to the state machine's message when nothing closed", () => {
    expect(describeConnectionFailure({ state: "failed", message: "host did not return ServerHello" })).toEqual({
      presentation: "strip",
      retryable: false,
      message: "host did not return ServerHello",
    });
  });
});

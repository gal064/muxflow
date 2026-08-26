// Handshake contract constants, copied from crates/protocol/src/lib.rs
// (PROTOCOL_MAJOR, PROTOCOL_MINOR, HOST_CAPABILITIES, CAPABILITY_NAMES,
// validate_host_contract). Keep them in sync with that file.

import type { ServerHello } from "./gen/envelope_pb";

export const PROTOCOL_MAJOR = 2;
export const PROTOCOL_MINOR = 0;

/** Every required capability, with the name a refusal reports it by (bits 0..16). */
export const CAPABILITY_NAMES: ReadonlyArray<readonly [bigint, string]> = [
  [1n << 0n, "snapshots"],
  [1n << 1n, "orderedEvents"],
  [1n << 2n, "cancellation"],
  [1n << 3n, "terminalStream"],
  [1n << 4n, "resync"],
  [1n << 5n, "tmuxActions"],
  [1n << 6n, "terminalResources"],
  [1n << 7n, "activeRoot"],
  [1n << 8n, "fileService"],
  [1n << 9n, "textEditor"],
  [1n << 10n, "bulkDownload"],
  [1n << 11n, "git"],
  [1n << 12n, "agents"],
  [1n << 13n, "terminalUpload"],
  [1n << 14n, "terminalOutputCredit"],
  [1n << 15n, "fileStream"],
  [1n << 16n, "terminalFileResolution"],
];

/** `HOST_CAPABILITIES` in crates/protocol/src/lib.rs: bits 0..16 all set (0x1FFFF = 131071). */
export const HOST_CAPABILITIES: bigint = CAPABILITY_NAMES.reduce((all, [bit]) => all | bit, 0n);

export function missingHostCapabilities(advertised: bigint): bigint {
  return HOST_CAPABILITIES & ~advertised;
}

export function capabilityNames(mask: bigint): string[] {
  const named = CAPABILITY_NAMES.filter(([bit]) => (mask & bit) !== 0n).map(([, name]) => name);
  if ((mask & ~HOST_CAPABILITIES) !== 0n) named.push("unknown");
  return named;
}

export type HostContractRefusal =
  | { kind: "protocolMajor"; advertised: number; message: string }
  | { kind: "readOnly"; incompatibility: string; message: string }
  | { kind: "missingCapabilities"; missing: bigint; message: string };

/**
 * The §7.3 admission checks, in order, with the exact user-facing copy. The
 * host's `expected_helper_version` check is not here: the phone sends an empty
 * string, which apps/host/src/service.rs treats as compatible.
 */
export function validateHostContract(envelopeMajor: number, hello: ServerHello): HostContractRefusal | undefined {
  if (envelopeMajor !== PROTOCOL_MAJOR) {
    return {
      kind: "protocolMajor",
      advertised: envelopeMajor,
      message: `This host's Muxflow helper speaks protocol v${envelopeMajor}; this app needs v${PROTOCOL_MAJOR}. Update the helper from the Muxflow desktop app.`,
    };
  }
  if (hello.readOnly) {
    return {
      kind: "readOnly",
      incompatibility: hello.incompatibility,
      message: `The Muxflow helper on this host is read-only: ${hello.incompatibility}. Update it from the Muxflow desktop app.`,
    };
  }
  const missing = missingHostCapabilities(hello.capabilities);
  if (missing !== 0n) {
    return {
      kind: "missingCapabilities",
      missing,
      message: `The Muxflow helper on this host is missing: ${capabilityNames(missing).join(", ")}. Update it from the Muxflow desktop app.`,
    };
  }
  return undefined;
}

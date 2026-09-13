// Constants generated and checked by crates/protocol/tests/mobile_contract.rs.
import type { ServerHello } from "./gen/envelope_pb";
import contract from "./gen/host_contract.json";

export const PROTOCOL_MAJOR = contract.protocolMajor;
export const PROTOCOL_MINOR = contract.protocolMinor;
export const HOST_CAPABILITIES = BigInt(contract.hostCapabilities);
export const CAPABILITY_NAMES: ReadonlyArray<readonly [bigint, string]> =
  contract.capabilities.map(({ bit, name }) => [BigInt(bit), name] as const);

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

// Constants generated and checked by crates/protocol/tests/mobile_contract.rs.
import contract from "./gen/host_contract.json";

// Admission requires exact major equality. Bump only if the previous released
// mobile app breaks/misbehaves with the new host, or the new app does so with
// the previous host. Harmless additive fields do not bump; no version branches.
export const PROTOCOL_MAJOR = contract.protocolMajor;
export type HostContractRefusal = { kind: "protocolMajor"; advertised: number; message: string };

/** Mobile updates independently; different majors are refused without retry. */
export function validateHostContract(envelopeMajor: number): HostContractRefusal | undefined {
  if (envelopeMajor !== PROTOCOL_MAJOR) {
    return {
      kind: "protocolMajor",
      advertised: envelopeMajor,
      message: `This host's Muxflow helper speaks protocol v${envelopeMajor}; this app needs v${PROTOCOL_MAJOR}. Update the app or the helper from Muxflow desktop.`,
    };
  }
  return undefined;
}

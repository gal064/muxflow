// Constants generated and checked by crates/protocol/tests/mobile_contract.rs.
import contract from "./gen/host_contract.json";

export const PROTOCOL_MAJOR = contract.protocolMajor;
export type HostContractRefusal = { kind: "protocolMajor"; advertised: number; message: string };

/** Mobile releases independently; only the exact current contract is admitted. */
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

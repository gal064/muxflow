Protocol tests pin framing failures, byte preservation, and assigned wire identities.
Generated Prost fields do not need individual Rust encode/decode echo tests.

`mobile_contract.rs` checks two committed Rust-produced JSON files: production
mobile constants and test-only admission/wire vectors. The mobile
`rustContract.test.ts` feeds those frames through its real decoder, checks raw
bytes and bigint identities, and encodes the exact Rust bytes again. Admission
vectors run the Rust validator and the mobile validator, including current-contract
acceptance and both older and newer major-version refusal. Regenerate intentional changes with
`pnpm --filter @muxflow/mobile protocol:gen`; ordinary Rust tests check for drift.

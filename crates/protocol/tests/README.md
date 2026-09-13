Protocol tests pin framing failures, byte preservation, and assigned wire identities.
Generated Prost fields do not need individual Rust encode/decode echo tests.

`mobile_contract.rs` checks two committed Rust-produced JSON files: production
mobile constants and test-only admission/wire vectors. The mobile
`rustContract.test.ts` feeds those frames through its real decoder, checks raw
bytes and bigint identities, and encodes the exact Rust bytes again. Admission
vectors run the Rust validator and the mobile validator, including precedence
and every missing required capability. Regenerate intentional changes with
`pnpm --filter @muxflow/mobile protocol:gen`; ordinary Rust tests check for drift.

Desktop terminal handshake and payload tests compile the `terminal_contract`
example in `tmux-control` and drive its real `PaneResourceStore`. Cargo must be
available when running these two Vitest files. The example supplies synthetic
capture bytes and event delivery; it does not test tmux capture or host service
routing. Those remain covered by Rust store/service tests and live host tests.
The desktop tests keep their real hub, reveal reducer, write scheduler and
generation watermark, with an xterm harness for deterministic render completion.

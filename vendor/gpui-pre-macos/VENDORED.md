# gpui-pre-macos 0.3.12

Vendored from crates.io, unmodified except for three calls in `src/window.rs`
and this file. Patched in through `[patch.crates-io]` in the root `Cargo.toml`.

**Why:** `gpui-kit` 0.10.0 pins `gpui-pre` 0.3.12, and this macOS backend calls
`Option::map_or_default`, an unstable method on the stable Rust 1.97.1 the
workspace pins.

**Change:** `.map_or_default(f)` is now `.map_or_else(Default::default, f)` at
three places in `src/window.rs`, marked by a comment at the top of that file.

**Remove it** (this directory, the `[patch.crates-io]` entry and the
`exclude` entry in the root `Cargo.toml`) when a released `gpui-pre-macos`
builds on stable Rust.

Upstream code is Apache-2.0, see `LICENSE-APACHE`.

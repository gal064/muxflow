# multiversion-macros 0.8.1

Vendored from crates.io, unmodified except for one change in
`src/dispatcher.rs`, the removal of the author address from `Cargo.toml`, and
this file. Only what the crate needs to build is kept: `src/`, `Cargo.toml` and
the license files. Patched in through `[patch.crates-io]` in the root
`Cargo.toml`.

**Why:** the proc macro collects the arches of a multiversioned function into a
`HashMap` and writes them out by iterating it, so the expansion order changes
with std's per-process random hash seed. `exr` (an `image` dependency, built on
x86 only) uses it, and the order reaches the final binary through LLVM's
per-module symbol suffixes: two builds of identical source on the same machine
produced different release desktop binaries, which the Linux package's
reproducibility check rejects.

**Change:** `HashMap` is now `BTreeMap` at that one site.

**Remove it** (this directory, the `[patch.crates-io]` entry and the `exclude`
entry in the root `Cargo.toml`) when a released `multiversion-macros` orders
its expansion deterministically.

Upstream code is MIT OR Apache-2.0, see the license files.

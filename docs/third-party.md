# Third-party notices

Every package ships one `THIRD-PARTY-LICENSES` file: the licence texts and
copyright notices of each Rust crate compiled into the app (`muxflow`) and the
helper (`muxflow-host`, including the Linux and macOS helpers a package
carries), and the licences of the fonts the app embeds. It sits at the
top of the Linux archive and is installed beside the binaries in
`lib/muxflow/`; on macOS it is `Muxflow.app/Contents/Resources/THIRD-PARTY-LICENSES`,
inside the signed bundle.

`release/third-party-licenses.mjs` writes it at package time from `Cargo.lock`,
so it is never stale. It runs `cargo-about` 0.9.3 (configured by
`release/about.toml`) once for the app and once for the helper, since it scans
one root crate at a time. It fails if a crate's licence cannot be determined.
Where a crate offers Apache-2.0, that text is shipped. Where a crate does not
package its licence file, the standard licence text is shipped with the
crate's authors, plus any other licence files the crate has. The Rust standard
library, which every binary includes, is named in the file's header. The packages' `verify-package.sh` scripts refuse a
package without the file. Install `cargo-about` with
`cargo install --locked --features cli cargo-about@0.9.3`.

The bundled JetBrains Mono fonts are licensed under the SIL Open Font License
1.1. Their license texts are in `apps/desktop/assets/fonts/LICENSE-JetBrainsMono.txt`
and `apps/mobile/assets/fonts/LICENSE.txt`.

The desktop app also bundles the Inter fonts, licensed under the SIL Open Font
License 1.1. Their license text is in `apps/desktop/assets/fonts/LICENSE-Inter.txt`.

The desktop app is built on GPUI (through `gpui-kit` and `gpui-pre`) and
`alacritty_terminal`, both under the Apache License 2.0. Their notices ship in
`THIRD-PARTY-LICENSES`; `cargo deny check licenses` keeps every dependency on the
license list in `deny.toml`.

The patches under `patches/` modify the mobile app's `@xterm/xterm` and
`expo-modules-jsi`. Both use MIT licenses; their copyright notices and license
texts are retained under `patches/licenses/`.

Voice setup downloads the `parakeet-unified-en-0.6b-Q8_0.gguf` model from the
[Handy conversion](https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf).
Model weights are not included in this repository. The conversion card labels
the weights CC-BY-4.0; the [upstream NVIDIA model card](https://huggingface.co/nvidia/parakeet-unified-en-0.6b)
states that the NVIDIA Open Model License Agreement governs the base model.
Review both cards and their terms before redistributing the weights.

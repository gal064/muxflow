# Third-party notices

The bundled JetBrains Mono fonts are licensed under the SIL Open Font License
1.1. Their license texts are in `apps/desktop/assets/fonts/LICENSE-JetBrainsMono.txt`
and `apps/mobile/assets/fonts/LICENSE.txt`.

The desktop app also bundles the Inter fonts, licensed under the SIL Open Font
License 1.1. Their license text is in `apps/desktop/assets/fonts/LICENSE-Inter.txt`.

The desktop app is built on GPUI (through `gpui-kit` and `gpui-pre`) and
`alacritty_terminal`, both under the Apache License 2.0. Their notices ship in
the crates' sources; `cargo deny check licenses` keeps every dependency on the
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

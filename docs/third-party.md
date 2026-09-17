# Third-party notices

The bundled JetBrains Mono fonts are licensed under the SIL Open Font License
1.1. Their license texts are in `apps/desktop/src/assets/fonts/LICENSE.txt` and
`apps/mobile/assets/fonts/LICENSE.txt`.

The patches under `patches/` modify `@xterm/xterm` and `@xterm/addon-webgl`.
Both packages use MIT licenses; their copyright notices and license texts are
retained under `patches/licenses/`.

Voice setup downloads the `parakeet-unified-en-0.6b-Q8_0.gguf` model from the
[Handy conversion](https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf).
Model weights are not included in this repository. The conversion card labels
the weights CC-BY-4.0; the [upstream NVIDIA model card](https://huggingface.co/nvidia/parakeet-unified-en-0.6b)
states that the NVIDIA Open Model License Agreement governs the base model.
Review both cards and their terms before redistributing the weights.

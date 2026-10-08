# Explorer: show ignored files button

Status: implemented after mock approval. Manual QA skipped at the user’s request.

Base: origin/main at d8a4c52. Branch: feat/explorer-gitignore-mock.

## Proposed interaction

- Add a 24px eye button to the right end of the explorer root header, beside the repository name and root metadata. It stays outside the scrolling file tree.
- Default: ignored entries are hidden. Neutral crossed-out eye; accessible name and tooltip: "Show ignored files".
- Enabled: blue eye with a subtle blue background; accessible name and tooltip: "Hide ignored files". Use aria-pressed to expose its state.
- Clicking toggles the existing showIgnored state. Keep the current context menu action, using the same state.
- Match existing eligibility: offer the button only when ignoredPaths contains entries. When git has no authoritative status, the existing explorer shows everything.
- Preserve the existing reset on root token or scope change. No persistence in this proposal.
- .gitignore is a configuration file, and stays visible unless Git reports it ignored. The button controls ignored files and directories, not dotfiles in general.
- Keep existing file row appearance, ordering, expansion, virtualization, and keyboard focus recovery.

## Implementation

1. In apps/desktop/src/ui/Icon.tsx, add eye and eyeOff paths to the existing icon set.
2. In apps/desktop/src/features/files/ExplorerTree.tsx, expose showIgnored through a header button; retain the context menu entry and shared state. Update the obsolete comment about having no header controls.
3. In apps/desktop/src/styles.css, add the compact header control and pressed/focus appearance using theme tokens; ensure long root names truncate before the button. Keep the existing Refreshing indicator readable.
4. Change the all-ignored empty-state help to point to the new button.
5. Extend existing ExplorerTree tests for button visibility, both toggle directions, shared context menu state, root/scope reset, and the all-ignored case. Verify focus recovery when the focused ignored row disappears, and check narrow headers and Refreshing layout.
6. Run the desktop test and check scripts. Manual QA is explicitly skipped.

## Wire impact

None. Reuses the existing frontend filtering and ignoredPaths data. No host or mobile changes, and no protocol-major change.

## Mock

Open explorer-gitignore-mock.html locally. Both panels are interactive and use the main branch's actual theme tokens and bundled fonts. explorer-gitignore-mock.png is the initial hidden/visible comparison.

## Validation

- Desktop TypeScript check passed.
- Full desktop suite: 161 test files and 1,726 tests passed.
- Independent diff review completed; no functional findings. The single P3 hover-color observation was rejected as cosmetic: the eye glyph and aria-pressed still expose the state.
- Manual QA skipped as requested.

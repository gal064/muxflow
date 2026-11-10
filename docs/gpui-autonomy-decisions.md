# GPUI migration: owner decisions

Status: decided. Phase 1 is next.

The language follows ASD-STE100 Simplified Technical English.

## Goal

The plan is done when the GPUI app works and has feature parity with the current
desktop app, except for the items in "Exceptions to parity". This occurs at the
end of Phase 6.

A pilot, measurements, Live Markdown, packaging and the removal of Tauri come
after that. You decide when these steps start. They do not stop the port.

## Words in this document

| Word | Meaning |
|---|---|
| Tauri | The framework of the current desktop app. |
| GPUI | The new native interface framework. |
| Phase | One part of the plan in `docs/gpui-migration.md`. |
| Feature parity | The new app does all that the current app does. |
| Freeze | No new features in the current app. Bug fixes continue. |
| CI | Automatic builds and tests on GitHub. |
| Live Markdown | The WYSIWYG Markdown editor. You edit the rendered text. |

## Decisions

| # | Decision | Answer |
|---|---|---|
| D1 | Start the port? | Yes. Agents do Phases 1 to 6 and merge the Mac test results (`gpui/s11`). |
| D2 | Keep the error marks and code completion of Monaco? | No. All other editor functions stay. |
| D3 | Keep spellcheck in Live Markdown? | No. |
| D4 | Freeze new desktop features? | Yes, from Phase 2 until the new app replaces Tauri. Bug fixes, host work and mobile work continue. |
| D5 | Can agents push `gpui/*` branches to GitHub? | Yes. Merges to `main`, tags and releases stay with you. |
| D6 | How do we test the Mac version? | CI builds it during the port. You run one prompt on your Mac at the end of Phase 6. |

The test at the end of Phase 6 includes IME input. The Mac must have an IME input
source. If it does not have one, you do that test in the pilot.

## Rule for scope

Keep feature parity, except when the complexity is large compared to the gain.

## Exceptions to parity

Agents record each exception in the ledger. We can add these items later.

| Item | Result |
|---|---|
| Monaco error marks and code completion | Not built. |
| Live Markdown spellcheck | Not built. |
| Live Markdown for large files | Not built. A measurement sets the size limit. Files above the limit open in the source editor. |
| Screen-reader access to terminal text and Live Markdown text | Not built. The terminal mirror and its Settings option go away. Buttons, tabs, menus and inputs keep their accessibility roles. |
| `MUXFLOW_NATIVE_DECORATIONS` and `MUXFLOW_WINDOW_CONTROLS` | Removed. The app continues to select its title bar automatically. |

## Live Markdown moves to Phase 7

GPUI Kit cannot hide Markdown characters or put tables inside the text. Thus Live
Markdown needs a custom editor. The work moves to Phase 7, after parity.

Until then, Markdown files open in the source editor with the rendered preview.
You decide after the pilot if you need Live Markdown. If you do, agents must build
it before Tauri goes, because the cutover removes the current Live Markdown.

Packaging, cutover and release become Phases 8, 9 and 10.

## Tests

Agents port the logic and state-machine tests. A small end-to-end set replaces
the view tests.

## Performance

Agents port the performance probes in Phase 3. The key-to-pixel tests outside the
app also stay. Performance numbers do not stop the port. You measure after parity.

## Items that do not need your decision

Agents use these defaults:

- **`gpui-fast`:** Off.
- **License exceptions:** Keep the current per-crate list in `deny.toml`. GPL and
  AGPL stay forbidden.
- **Mac build failure:** Use a newer `gpui-pre-macos` version if one compiles. If
  not, keep a small patch of the three failed calls.
- **`vt-parity-pause-idle` test:** This test fails on the current app path too.
  It does not stop the port. Agents record it as a separate problem.
- **Linux display:** Agents use isolated test displays. Agents do not use other
  windows.

## Why agents stop

Agents stop and tell you if one of these occurs:

1. A change to the wire protocol is necessary.
2. A dependency has a license that the plan does not allow.
3. A feature cannot reach parity and needs a new exception.
4. A test continues to fail after three fix attempts.
5. A task must touch a process or container that it did not start.

## After the port

These steps stay with you. They are not part of Phases 1 to 6:

1. Use the new app in a pilot.
2. Measure speed and memory.
3. Decide if agents build Live Markdown.
4. Approve packaging and the pull request that removes Tauri.
5. Run the release.

Known items to check in the pilot: on the Mac, the spike typed approximately
15 ms slower than the current app. Some spike resize tests also had a timeout.
Phase 3 must make resize work during heavy output.

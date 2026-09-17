# Codex `request_user_input` should surface as blocked

## Outcome

When Codex presents a `request_user_input` question, Muxflow marks that agent
`Blocked` and emits the existing blocked-attention alert. Answering or
dismissing the question moves the agent back to `Working`; the later `Stop`
continues to produce the normal completed alert.

No question text, choices, tool input, transcript contents, or credentials are
retained or sent through the daemon. The only new normalized datum is the exact
tool-name discriminator needed to classify the lifecycle event.

## Evidence

- Codex 0.149.1 session `01a03b93-36ca-7b02-9e6b-821c2e8e918c` emitted three
  matching `PreToolUse(request_user_input)` / `PostToolUse(request_user_input)`
  pairs during a sanitized live capture.
- Bash calls in that session occurred before the questions and after the final
  answer, not while a question was waiting.
- Muxflow and another local app were already registered for `PreToolUse`; registration
  was not the failure.
- Muxflow's ingress retained the event name but discarded `tool_name`, after
  which every `PreToolUse` was classified as `Working`.
- The existing reducer already turns a transition to `Blocked` into blocked
  attention with `notify = true`.
- Temporary instrumentation in the other app was removed, its original digest
  restored, and the capture file deleted before implementation.

## Implementation

- Preserve a non-empty `tool_name` only for normalized Codex `PreToolUse`
  events. Continue discarding `tool_input`, questions, answers, prompts,
  transcripts, and arbitrary vendor metadata.
- Map exact `PreToolUse` + `request_user_input` to `Blocked`. Ordinary,
  missing, malformed, and future tool names remain `Working`.
- Keep `PostToolUse` mapped to `Working`, so the question returning clears the
  blocked lifecycle state without another alert.
- Keep the hook event set, managed version, protocol schema, UI, and
  notification copy unchanged.

## Verification and QA

- Boundary coverage asserts the normalized envelope contains only the event,
  session, and tool-name discriminator when the vendor payload also contains
  sentinel-private question data.
- Adapter coverage pins exact matching and safe behavior for ordinary,
  missing, and malformed tool names.
- Lifecycle coverage pins one blocked attention transition, no duplicate
  attention while already blocked, working after `PostToolUse`, and normal
  completion after `Stop`.
- Host-only manual QA uses isolated HOME, runtime, and tmux directories. The
  real hook-ingest path must produce Working (1), Blocked (2), Working (1), and
  Idle (3) for prompt, question, answer, and Stop respectively; ordinary Bash
  remains Working.
- Sentinel-private data must be absent from both live persisted state and an
  offline fallback envelope while `request_user_input` remains present as the
  discriminator.

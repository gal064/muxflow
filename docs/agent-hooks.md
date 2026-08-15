# Codex and Claude Code hooks

Codex and Claude Code are the only V1 agent adapters. The app can launch them or
detect manually started processes. Installed hooks improve lifecycle authority;
process and screen detection remain bounded fallbacks.

Hook installation is always a separate, reviewable action from remote-helper
installation. The review shows every touched file, before/after hashes, a
redacted diff, the exact managed command, and adapter trust guidance. Confirming
merges labeled entries while preserving unrelated configuration and creates a
backup. Repeating install or upgrade is idempotent.

## Whether this host reports anything

Every agent snapshot carries, per adapter, what this host's configuration
actually does with lifecycle events — observed by the daemon, not claimed by the
adapter:

| State | Meaning |
| --- | --- |
| `wired` | Every event this adapter needs invokes the managed hook, at the current owned version. |
| `partial` | Owned entries exist but do not cover every event; some transitions can never arrive. |
| `notWired` | The agent is here and nothing routes its events to this app. |
| `absent` | The agent is not on this host: no configuration, no directory, nothing on `PATH`. |
| `unavailable` | The configuration could not be inspected. Deliberately not the same as absent — it is never written over. |
| `unspecified` | The host has not answered: no snapshot yet, or a helper too old to have an opinion. Never treated as a definite answer. |

On connect the desktop asks once per host profile whether to set it up, and only
when the host reports nothing at all. The answer is remembered either way, and
`Settings → Connection → Set up agent status…` and the agents section's context
menu are where it is revisited. Consent is to keeping the host set up: when the
managed event set grows, an already-consented host is brought current on connect
without asking again — merge-only, backed up and idempotent, so a host that is
already current is not written to.

The same installer is reachable without a UI, which is how it is exercised
against a copy of a real configuration:

```
tmux-ide-host hook status    [--adapter codex|claude-code] [--home DIR] [--settings-path FILE]
tmux-ide-host hook install   [--adapter …] [--home …] [--settings-path …] [--yes]
tmux-ide-host hook uninstall [--adapter …] [--home …] [--settings-path …] [--yes]
```

`install` and `uninstall` refuse to change the agent configuration in your own
home directory unless you pass `--yes`. The desktop will not write a host's
configuration without a recorded answer for that host, and this command is the
same installer without an interface to ask through: `--yes` is where the answer
goes. A run redirected by `--home` or `--settings-path` changes nothing of
yours and needs no confirmation. `status` reads only, and is never gated.

`--home` relocates every adapter's configuration; `--settings-path` relocates
exactly the adapter named by `--adapter`, which it requires. Without
`--adapter`, install acts on every adapter that is actually on the host, and
uninstall on every adapter that has entries of this app's to remove. Either
verb reports every adapter it did not act on and why, and exits non-zero if any
adapter failed — after printing what the others did.

## Events taken, and the gaps

Claude Code: `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
`PermissionRequest`, `PostToolUse`, `SubagentStop`, `Stop`, `StopFailure`,
`Notification`.

Codex: the same, minus `StopFailure` and `Notification`, which its hook surface
does not have (measured against Codex CLI 0.128 and 0.147). A Codex turn that
ends in failure is therefore indistinguishable from one that succeeds, and
`PermissionRequest` is the only evidence of a blocked Codex agent. Recorded
rather than faked.

A `Working` state that receives no further event for fifteen minutes decays to
`Unknown`. Attention already earned survives; only the claim about right now is
dropped. The bound is the longest gap a healthy agent can leave — one tool call,
at Claude's maximum configurable `Bash` timeout of 600s — with half again for
headroom.

Use the same review flow with Uninstall to remove only entries labeled
`tmux-agent-ide-managed`. Do this before uninstalling the desktop package. The
package uninstaller refuses to remove the helper while a known managed marker
is present, preventing silently broken agent configuration.

Hooks submit compact state JSON through the private daemon socket and never send
prompt text, terminal output, file contents, or credentials. Malformed or stale
events are rejected; a temporarily unavailable daemon retains only a bounded,
atomic latest-state record.

For validation, use installed `codex --version` / `codex --help` and
`claude --version` / `claude --help` only. Release QA must not send prompts or
perform network-backed agent work with the user's accounts.

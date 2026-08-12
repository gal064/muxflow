# Codex and Claude Code hooks

Codex and Claude Code are the only V1 agent adapters. The app can launch them or
detect manually started processes. Installed hooks improve lifecycle authority;
process and screen detection remain bounded fallbacks.

Hook installation is always a separate, reviewable action from remote-helper
installation. The review shows every touched file, before/after hashes, a
redacted diff, the exact managed command, and adapter trust guidance. Confirming
merges labeled entries while preserving unrelated configuration and creates a
backup. Repeating install or upgrade is idempotent.

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

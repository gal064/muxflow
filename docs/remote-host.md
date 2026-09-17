# Remote hosts and the managed helper

Remote connections use the system OpenSSH client. Authentication prompts,
known-host verification, agent forwarding policy, `ProxyJump`, and key choice
remain OpenSSH responsibilities. The app creates a private temporary control
socket and uses separate non-multiplexed bulk connections so transfers do not
starve terminal and notification traffic. No public or TCP listener is opened.

## Helper lifecycle

The remote `muxflow-host` binary is installed under
`~/.local/bin/muxflow-host` only after explicit confirmation. Installation:

1. probes OS and architecture;
2. chooses the matching x86-64 or ARM64 artifact;
3. uploads to a private temporary path;
4. verifies SHA-256 and architecture;
5. atomically replaces the managed binary; and
6. restores the previous helper if the new handshake fails.

An incompatible helper makes the UI read-only until an explicit upgrade
succeeds. Mutations created before a disconnect are never replayed.

## Remote requirements

- Linux x86-64 for the validated package; ARM64 only after producing and
  validating the native ARM64 artifact described in the release guide
- tmux 3.3 or newer
- Git for Git features
- a writable home directory
- OpenSSH access as the normal user

The daemon socket and runtime files are user-only. A daemon restart does not
restart tmux. If remote setup fails, run the packaged helper's `doctor` command
locally on that host and generate a redacted support bundle as described in
[`diagnostics-privacy-security.md`](diagnostics-privacy-security.md).

Use a UTF-8 locale when session/window names contain Unicode. tmux under a
minimal POSIX locale may normalize non-ASCII characters; names containing
spaces are supported. Session and window renames are bidirectional and
authoritative after reconnect. Pane titles remain owned by tmux/terminal apps.

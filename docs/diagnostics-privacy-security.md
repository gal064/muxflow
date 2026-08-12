# Diagnostics, privacy, and security

The host provides two local diagnostics commands:

```sh
tmux-ide-host doctor
tmux-ide-host doctor --json
tmux-ide-host support-bundle --output ./tmux-agent-ide-support.json
```

`doctor` checks the installed helper, protocol and capability versions, platform, tmux/Git/OpenSSH availability, the private Unix-socket endpoint, configured flow limits, runtime connection counters, and recent safe error classes. The support bundle is the same schema-versioned report in a portable JSON file.

The support bundle is local-only. It is never uploaded automatically and deliberately has no fields for terminal output, prompts, file contents, notification contents, environment values, SSH configuration, credentials, usernames, hostnames, or filesystem paths. Dependency probes run only their version flags; SSH configuration and authentication are not read. Recent errors are fixed classes and counts, not error messages.

Bundles are created as new user-only files (`0600`). Existing files and symlink destinations are refused. Runtime state is bounded, strict-schema JSON under the existing user-only runtime directory; unsafe permissions, symlinks, unknown fields, and unsupported schemas are ignored or safely replaced without copying their contents into a report. The daemon listens only on its private Unix socket and diagnostics do not add a TCP listener.

Review a bundle before sharing it. Although the schema excludes private content by construction, timestamps and aggregate operational counters may still reveal when and how often the app was used.

## Runtime security expectations

- Authentication and host-key verification remain OpenSSH responsibilities; the app does not copy or store private keys or passwords.
- The daemon runtime directory is mode `0700`; its socket, metadata, and diagnostics state are user-only.
- The support-bundle command never overwrites a destination. Choose a new output filename and delete it after support work if it is no longer needed.
- No telemetry is sent in v1.

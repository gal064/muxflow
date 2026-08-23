# Upgrade and uninstall

Before uninstalling, use the agent-hook review UI to uninstall managed Codex and
Claude Code hooks. This removes only labeled app-owned entries and preserves all
unrelated configuration.

Run the installed uninstaller:

```sh
~/.local/lib/muxflow/uninstall.sh
```

Set `ADE_INSTALL_PREFIX` if the package was installed elsewhere. The script
stops only the app-owned helper daemon and removes package-owned binaries,
launcher, desktop entry, and icon. It does not kill tmux servers or sessions and
does not remove host profiles, editor tabs, preferences, SSH configuration,
agent configuration, or other user data.

If managed hook markers remain, uninstall stops with instructions instead of
leaving broken hook commands. Remove the hooks through the app and retry.

To remove local app state later, inspect and manually delete the application's
configuration/state directory. That separate destructive step is intentionally
not part of package uninstall.
# macOS

Run `release/macos/uninstall.sh`. It removes only the ownership-marked app,
stops its local helper when possible, and preserves application configuration
and all tmux sessions.

With no argument it looks in both places the installer can publish to,
`$HOME/Applications` (the default) and `/Applications` (the machine-wide
opt-in), and removes whichever one holds the app. If neither does it says so
and names both paths. If both do it refuses rather than guessing; pass the
directory to act on:

```sh
release/macos/uninstall.sh /Applications
```

`ADE_MACOS_APPLICATIONS_DIR` selects a single directory the same way, and an
explicit argument takes precedence over it.

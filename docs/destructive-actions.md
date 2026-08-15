# Destructive actions and recovery limits

The desktop requires confirmation before killing a tmux session, deleting a
non-empty directory, overwriting a file, discarding Git changes, or pasting an
upload larger than 500 MiB. The confirmation identifies the exact target and is
revalidated against the current server/root generation before the mutation is
sent.

Closing a single terminal tab or pane is deliberately not one of them: it is the
surface the user is looking at and the result is visible the instant it happens.
The wire contract is unchanged — those actions still carry `confirmed` and the
authoritative precondition, captured when the command ran. Only the dialog is
gone, and only for those two.

After reconnect, stale mutations are rejected rather than replayed. File and
Git requests are scoped to an authoritative root identity. Path traversal,
directory-symlink traversal, stale hunks, and destination replacement are
rejected.

Git discard and last-writer-wins editor reload can be unrecoverable when content
is not committed or backed up. Autosave minimizes the conflict window but is not
a merge system. Review the target and use normal Git history/backups for
recovery.

Cancelled or failed transfers do not publish a complete-looking destination.
The app verifies byte count and BLAKE3 before atomic publication and cleans only
partials it owns. Do not manually rename files from the private staging area.

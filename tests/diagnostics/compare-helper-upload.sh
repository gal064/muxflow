#!/usr/bin/env bash
# Compare the installer's upload transport using two packaged Linux helpers.
# Accepts .app bundles or raw artifact paths. Never invokes install/daemon-stop.
set -euo pipefail
if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 SSH_TARGET BEFORE_APP_OR_HELPER AFTER_APP_OR_HELPER [SSH_CONFIG]" >&2
  exit 2
fi
target=$1
before=$2
after=$3
[[ -n "$target" && "$target" != -* && "$target" != *[[:space:]]* ]] || exit 2
ssh_args=()
if [[ $# == 4 ]]; then ssh_args+=(-F "$4"); fi
# Match SshControl::base_command and upload_independent; do not tune away a failure.
ssh_args+=(-T -o BatchMode=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ControlMaster=no -o ControlPath=none)
evidence=$(mktemp -d "${TMPDIR:-/tmp}/muxflow-upload-evidence.XXXXXXXX")
echo "Evidence: $evidence"
architecture=$(ssh "${ssh_args[@]}" "$target" 'uname -m')
case "$architecture" in x86_64|aarch64) ;; *) echo "Unsupported remote architecture: $architecture" >&2; exit 1 ;; esac
artifact_for() {
  if [[ -f "$1" ]]; then printf '%s\n' "$1"; return; fi
  local candidate
  for candidate in "$1/Contents/Resources/muxflow-host-linux-$architecture" "$1/Contents/MacOS/muxflow-host-linux-$architecture"; do
    if [[ -f "$candidate" ]]; then printf '%s\n' "$candidate"; return; fi
  done
  echo "No packaged Linux helper found in $1" >&2
  return 1
}
before_artifact=$(artifact_for "$before")
after_artifact=$(artifact_for "$after")
remote_dir=$(ssh "${ssh_args[@]}" "$target" 'mktemp -d /tmp/muxflow-upload-check.XXXXXXXX')
[[ "$remote_dir" =~ ^/tmp/muxflow-upload-check\.[a-zA-Z0-9]+$ ]] || { echo 'Unexpected temporary path' >&2; exit 1; }
cleanup() {
  ssh "${ssh_args[@]}" "$target" "rm -f '$remote_dir/before' '$remote_dir/after'; rmdir '$remote_dir'" >/dev/null 2>&1 || echo "Temporary upload files remain at $remote_dir" >&2
}
trap cleanup EXIT
failed=0
for label in before after; do
  artifact=$before_artifact
  if [[ "$label" == after ]]; then artifact=$after_artifact; fi
  bytes=$(wc -c < "$artifact" | tr -d ' ')
  if command -v sha256sum >/dev/null; then digest=$(sha256sum "$artifact" | cut -d' ' -f1)
  else digest=$(shasum -a 256 "$artifact" | cut -d' ' -f1); fi
  started=$SECONDS
  echo "$label: $bytes bytes, sha256=$digest" | tee "$evidence/$label.txt"
  # Verbose stderr is saved even if SSH closes its input during the upload.
  if ssh -v "${ssh_args[@]}" "$target" "cat > '$remote_dir/$label'" < "$artifact" 2>"$evidence/$label.ssh.log"; then
    remote_digest=$(ssh "${ssh_args[@]}" "$target" "sha256sum '$remote_dir/$label'" | cut -d' ' -f1)
    if [[ "$remote_digest" == "$digest" ]]; then
      echo "$label: upload verified in $((SECONDS - started))s" | tee -a "$evidence/$label.txt"
    else
      echo "$label: digest mismatch" | tee -a "$evidence/$label.txt"
      failed=1
    fi
  else
    result=$?
    echo "$label: SSH exited $result after $((SECONDS - started))s" | tee -a "$evidence/$label.txt"
    tail -20 "$evidence/$label.ssh.log"
    failed=1
  fi
done
exit "$failed"

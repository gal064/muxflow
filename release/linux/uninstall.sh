#!/usr/bin/env bash
set -euo pipefail

prefix=${ADE_INSTALL_PREFIX:-"$HOME/.local"}
owner='dev.muxflow.desktop:1'
[[ "$prefix" == /* && "$prefix" != / ]]
current=/
IFS=/ read -r -a prefix_parts <<< "${prefix#/}"
for part in "${prefix_parts[@]}"; do
  [[ -n "$part" ]] || continue
  current="${current%/}/$part"
  [[ -d "$current" && ! -L "$current" ]] || {
    echo "uninstall prefix contains a symlinked or non-directory ancestor: $current" >&2
    exit 73
  }
done
prefix=$(realpath -e "$prefix")
lib_dir="$prefix/lib/muxflow"
desktop_link="$prefix/bin/muxflow"
host_link="$prefix/bin/muxflow-host"
desktop_file="$prefix/share/applications/muxflow.desktop"
icon_file="$prefix/share/icons/hicolor/256x256/apps/muxflow.png"
install_directories=(
  "$prefix/lib" "$prefix/bin" "$prefix/share" "$prefix/share/applications"
  "$prefix/share/icons" "$prefix/share/icons/hicolor"
  "$prefix/share/icons/hicolor/256x256"
  "$prefix/share/icons/hicolor/256x256/apps"
)
for directory in "$prefix" "${install_directories[@]}"; do
  [[ -d "$directory" && ! -L "$directory" ]] || {
    echo "uninstall destination contains a symlinked or non-directory ancestor: $directory" >&2
    exit 73
  }
done

[[ -d "$lib_dir" && ! -L "$lib_dir" && -f "$lib_dir/.package-owner" ]]
grep -Fxq "$owner" "$lib_dir/.package-owner"
while IFS= read -r installed; do
  case "${installed#"$lib_dir/"}" in
    muxflow|muxflow-host|muxflow-host-x86_64|muxflow-host-aarch64|uninstall.sh|SHA256SUMS|.package-owner|.owned-assets|.created-dirs) ;;
    *) echo "refusing to remove unowned file from package directory: $installed" >&2; exit 73 ;;
  esac
done < <(find "$lib_dir" -mindepth 1 -type f -print)
[[ -z "$(find "$lib_dir" -mindepth 1 ! -type f -print -quit)" ]]
[[ -L "$desktop_link" && "$(readlink "$desktop_link")" == ../lib/muxflow/muxflow ]]
[[ -L "$host_link" && "$(readlink "$host_link")" == ../lib/muxflow/muxflow-host ]]
[[ -f "$desktop_file" && ! -L "$desktop_file" && -f "$icon_file" && ! -L "$icon_file" ]]
read -r desktop_digest icon_digest < "$lib_dir/.owned-assets"
mapfile -t created_directories < "$lib_dir/.created-dirs"
for directory in "${created_directories[@]}"; do
  case "$directory" in
    "$prefix"|"${install_directories[0]}"|"${install_directories[1]}"|"${install_directories[2]}"|"${install_directories[3]}"|"${install_directories[4]}"|"${install_directories[5]}"|"${install_directories[6]}"|"${install_directories[7]}") ;;
    *) echo "refusing unsafe created-directory record: $directory" >&2; exit 73 ;;
  esac
done
[[ "$(sha256sum "$desktop_file" | cut -d' ' -f1)" == "$desktop_digest" ]]
[[ "$(sha256sum "$icon_file" | cut -d' ' -f1)" == "$icon_digest" ]]

if ! hook_status=$("$lib_dir/muxflow-host" hooks-status 2>/dev/null); then
  echo "Could not safely inspect managed agent hooks; refusing uninstall." >&2
  echo "Remove hooks from the app or repair their configuration, then retry." >&2
  exit 73
fi
if grep -Fq '"managedHooksPresent":true' <<<"$hook_status"; then
  echo "Managed Codex or Claude Code hooks are still installed." >&2
  echo "Uninstall them from the app first, then rerun this script." >&2
  exit 73
fi

if [[ -n "${ADE_HOST_RUNTIME_DIR:-}" ]]; then
  runtime=$ADE_HOST_RUNTIME_DIR
elif [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
  runtime="$XDG_RUNTIME_DIR/muxflow"
else
  runtime="/tmp/muxflow-$(id -u)"
fi
socket="$runtime/host.sock"
if [[ -S "$socket" ]]; then
  "$lib_dir/muxflow-host" daemon-stop
  for _ in $(seq 1 100); do
    [[ ! -S "$socket" ]] && break
    sleep 0.02
  done
  [[ ! -S "$socket" ]] || { echo "host daemon did not stop; refusing uninstall" >&2; exit 73; }
fi

quarantine=$(mktemp -d "$prefix/lib/.muxflow.uninstall.XXXXXX")
mv "$lib_dir" "$quarantine/package"
if ! rm -f "$desktop_link" "$host_link" "$desktop_file" "$icon_file"; then
  mv "$quarantine/package" "$lib_dir"
  rmdir "$quarantine"
  echo "uninstall publication failed; package was restored" >&2
  exit 73
fi
rm -rf "$quarantine"
for ((index=${#created_directories[@]} - 1; index >= 0; index--)); do
  rmdir "${created_directories[$index]}" >/dev/null 2>&1 || true
done

printf 'Removed Muxflow binaries. tmux sessions and user configuration were preserved.\n'

#!/usr/bin/env bash
set -euo pipefail

package_root=$(cd "$(dirname "$0")" && pwd)
prefix=${ADE_INSTALL_PREFIX:-"$HOME/.local"}
owner='dev.muxflow.desktop:1'

case "$prefix" in
  /*) ;;
  *) echo "install prefix must be absolute" >&2; exit 64 ;;
esac
case "$prefix" in
  /|*$'\n'*|*$'\r'*) echo "install prefix is unsafe" >&2; exit 64 ;;
esac

(cd "$package_root" && sha256sum -c SHA256SUMS >/dev/null)
prefix_created=false
[[ -e "$prefix" || -L "$prefix" ]] || prefix_created=true
current=/
IFS=/ read -r -a prefix_parts <<< "${prefix#/}"
for part in "${prefix_parts[@]}"; do
  [[ -n "$part" ]] || continue
  current="${current%/}/$part"
  if [[ -e "$current" || -L "$current" ]]; then
    [[ -d "$current" && ! -L "$current" ]] || {
      echo "install prefix contains a symlinked or non-directory ancestor: $current" >&2
      exit 73
    }
  else
    mkdir "$current"
  fi
done
prefix=$(realpath -e "$prefix")
[[ "$prefix" != / ]]

lib_parent="$prefix/lib"
lib_dir="$lib_parent/muxflow"
bin_dir="$prefix/bin"
application_dir="$prefix/share/applications"
icon_dir="$prefix/share/icons/hicolor/256x256/apps"
desktop_file="$application_dir/muxflow.desktop"
icon_file="$icon_dir/muxflow.png"
desktop_link="$bin_dir/muxflow"
host_link="$bin_dir/muxflow-host"
created_directories=()
$prefix_created && created_directories+=("$prefix")
install_directories=( \
  "$lib_parent" "$bin_dir" "$prefix/share" "$application_dir" \
  "$prefix/share/icons" "$prefix/share/icons/hicolor" \
  "$prefix/share/icons/hicolor/256x256" "$icon_dir"
)
assert_install_directories() {
  local directory
  for directory in "$prefix" "${install_directories[@]}"; do
    if [[ -e "$directory" || -L "$directory" ]]; then
      [[ -d "$directory" && ! -L "$directory" ]] || {
        echo "install destination contains a symlinked or non-directory ancestor: $directory" >&2
        return 73
      }
    fi
  done
}
assert_install_directories
for directory in "${install_directories[@]}"; do
  if [[ ! -e "$directory" ]]; then
    mkdir "$directory"
    created_directories+=("$directory")
  fi
done
assert_install_directories

upgrading=false
if [[ -e "$lib_dir" || -L "$lib_dir" ]]; then
  [[ -d "$lib_dir" && ! -L "$lib_dir" && -f "$lib_dir/.package-owner" ]] || {
    echo "refusing to replace an installation not owned by Muxflow" >&2; exit 73;
  }
  grep -Fxq "$owner" "$lib_dir/.package-owner" || {
    echo "installed package ownership marker is invalid" >&2; exit 73;
  }
  upgrading=true
else
  for destination in "$desktop_link" "$host_link" "$desktop_file" "$icon_file"; do
    [[ ! -e "$destination" && ! -L "$destination" ]] || {
      echo "refusing to overwrite pre-existing path: $destination" >&2; exit 73;
    }
  done
fi

if $upgrading; then
  [[ -L "$desktop_link" && "$(readlink "$desktop_link")" == ../lib/muxflow/muxflow ]]
  [[ -L "$host_link" && "$(readlink "$host_link")" == ../lib/muxflow/muxflow-host ]]
  [[ -f "$desktop_file" && ! -L "$desktop_file" && -f "$icon_file" && ! -L "$icon_file" ]]
  read -r old_desktop_digest old_icon_digest < "$lib_dir/.owned-assets"
  [[ "$(sha256sum "$desktop_file" | cut -d' ' -f1)" == "$old_desktop_digest" ]]
  [[ "$(sha256sum "$icon_file" | cut -d' ' -f1)" == "$old_icon_digest" ]]
fi

# Recheck immediately before staging and again before publication. This is a
# fail-closed defense against accidental path replacement during an upgrade;
# the installer never follows a pre-existing symlinked install ancestor.
assert_install_directories

umask 077
transaction=$(mktemp -d "$lib_parent/.muxflow.transaction.XXXXXX")
stage_dir="$transaction/new-lib"
backup_dir="$transaction/old-lib"
mkdir -p "$stage_dir"
published=false
cleanup() {
  if $published; then
    rm -f "$desktop_link" "$host_link" "$desktop_file" "$icon_file"
    rm -rf "$lib_dir"
    if [[ -d "$backup_dir" ]]; then
      mv "$backup_dir" "$lib_dir"
      ln -s ../lib/muxflow/muxflow "$desktop_link"
      ln -s ../lib/muxflow/muxflow-host "$host_link"
      cp -a "$transaction/old-desktop" "$desktop_file"
      cp -a "$transaction/old-icon" "$icon_file"
    fi
  fi
  rm -rf "$transaction"
}
trap cleanup EXIT

install -m 0755 "$package_root/bin/muxflow" "$stage_dir/muxflow"
install -m 0755 "$package_root/bin/muxflow-host" "$stage_dir/muxflow-host"
for qualified in "$package_root"/bin/muxflow-host-x86_64 "$package_root"/bin/muxflow-host-aarch64; do
  [[ -f "$qualified" ]] && install -m 0755 "$qualified" "$stage_dir/$(basename "$qualified")"
done
install -m 0755 "$package_root/uninstall.sh" "$stage_dir/uninstall.sh"
install -m 0644 "$package_root/SHA256SUMS" "$stage_dir/SHA256SUMS"
printf '%s\n' "$owner" > "$stage_dir/.package-owner"
if $upgrading && [[ -f "$lib_dir/.created-dirs" ]]; then
  cp "$lib_dir/.created-dirs" "$stage_dir/.created-dirs"
else
  printf '%s\n' "${created_directories[@]}" > "$stage_dir/.created-dirs"
fi

desktop_stage="$transaction/new-desktop"
while IFS= read -r line || [[ -n "$line" ]]; do
  case "$line" in
    'Exec=@EXEC@')
      value="$desktop_link"; value=${value//\\/\\\\}; value=${value//\"/\\\"}; value=${value//\`/\\\`}; value=${value//\$/\\$}
      printf 'Exec="%s"\n' "$value" ;;
    'Icon=@ICON@') printf 'Icon=%s\n' "$icon_file" ;;
    *) printf '%s\n' "$line" ;;
  esac
done < "$package_root/share/applications/muxflow.desktop.in" > "$desktop_stage"
icon_stage="$transaction/new-icon"
install -m 0644 "$package_root/share/icons/hicolor/256x256/apps/muxflow.png" "$icon_stage"
chmod 0644 "$desktop_stage"
printf '%s %s\n' \
  "$(sha256sum "$desktop_stage" | cut -d' ' -f1)" \
  "$(sha256sum "$icon_stage" | cut -d' ' -f1)" > "$stage_dir/.owned-assets"

if $upgrading; then
  cp -a "$desktop_file" "$transaction/old-desktop"
  cp -a "$icon_file" "$transaction/old-icon"
  mv "$lib_dir" "$backup_dir"
fi
mv "$stage_dir" "$lib_dir"
published=true
if [[ "${ADE_PHASE8_TEST_FAIL_AFTER_LIB_PUBLICATION:-}" == 1 ]]; then
  echo "injected package publication failure" >&2
  false
fi

assert_install_directories

link_stage="$transaction/desktop-link"
ln -s ../lib/muxflow/muxflow "$link_stage"
mv -Tf "$link_stage" "$desktop_link"
link_stage="$transaction/host-link"
ln -s ../lib/muxflow/muxflow-host "$link_stage"
mv -Tf "$link_stage" "$host_link"
mv -Tf "$desktop_stage" "$desktop_file"
mv -Tf "$icon_stage" "$icon_file"

published=false
rm -rf "$backup_dir" "$transaction"
trap - EXIT
printf 'Installed Muxflow under %s\n' "$prefix"

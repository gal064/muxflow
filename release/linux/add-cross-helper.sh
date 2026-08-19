#!/usr/bin/env bash
set -euo pipefail

archive=${1:?usage: add-cross-helper.sh TARGET_ARCHIVE OTHER_ARCHIVE}
other_archive=${2:?usage: add-cross-helper.sh TARGET_ARCHIVE OTHER_ARCHIVE}
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
source_date_epoch=${SOURCE_DATE_EPOCH:-1704067200}
release/linux/verify-package.sh "$archive" >/dev/null
release/linux/verify-package.sh "$other_archive" >/dev/null
work=$(mktemp -d "$repo_root/tmp/linux-cross-helper.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/target" "$work/other"
tar -xzf "$archive" -C "$work/target"
tar -xzf "$other_archive" -C "$work/other"
target_root=$(find "$work/target" -mindepth 1 -maxdepth 1 -type d -print -quit)
other_root=$(find "$work/other" -mindepth 1 -maxdepth 1 -type d -print -quit)
case "$(basename "$other_archive")" in
  *-linux-x86_64.tar.gz) other_arch=x86_64 ;;
  *-linux-aarch64.tar.gz) other_arch=aarch64 ;;
  *) echo "other archive has unsupported architecture" >&2; exit 64 ;;
esac
install -m 0755 "$other_root/bin/tmux-ide-host" "$target_root/bin/tmux-ide-host-$other_arch"
(
  cd "$target_root"
  find . -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > SHA256SUMS
)
parent=$(dirname "$archive")
name=$(basename "$archive")
package_name=$(basename "$target_root")
tar --sort=name --mtime="@$source_date_epoch" --owner=0 --group=0 --numeric-owner \
  --pax-option=delete=atime,delete=ctime -C "$work/target" -cf - "$package_name" | gzip -n > "$archive.new"
mv "$archive.new" "$archive"
(cd "$parent" && sha256sum "$name") > "$archive.sha256"
release/linux/verify-package.sh "$archive" >/dev/null

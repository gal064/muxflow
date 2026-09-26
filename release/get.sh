#!/usr/bin/env bash
# One-line Linux and macOS installer, published on every release as `install.sh`:
#
#   curl -fsSL https://github.com/gal064/muxflow/releases/latest/download/install.sh | bash
#
# Downloads the release package for this machine and checks it against the
# release SHA256SUMS. On Linux it runs the tarball's own install.sh; on macOS
# it copies Muxflow.app out of the DMG into /Applications. Running it again
# upgrades in place. MUXFLOW_VERSION pins a release; ADE_INSTALL_PREFIX (Linux)
# is passed through to the package installer, and ADE_MACOS_APPLICATIONS_DIR
# (macOS) picks another applications folder.
set -euo pipefail

releases=${MUXFLOW_RELEASES_URL:-https://github.com/gal064/muxflow/releases}
owner='dev.muxflow.desktop:1'

work=
mounted=
cleanup() {
  [[ -z "$mounted" ]] || hdiutil detach -quiet "$mounted" || true
  [[ -z "$work" ]] || rm -rf "$work"
}
trap cleanup EXIT

fail() {
  printf 'muxflow: %s\n' "$*" >&2
  exit 1
}

fetch() {
  if command -v curl >/dev/null; then
    curl -fsSL -o "$2" "$1" || fail "download failed: $1"
  elif command -v wget >/dev/null; then
    wget -qO "$2" "$1" || fail "download failed: $1"
  else
    fail "curl or wget is required"
  fi
}

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Downloads release asset $1 of version $2 into $work and checks its checksum.
fetch_verified() {
  local name=$1 base="$releases/download/v$2"
  fetch "$base/$name" "$work/$name"
  fetch "$base/SHA256SUMS" "$work/SHA256SUMS"
  local expected actual
  expected=$(awk -v name="$name" '$2 == name { print $1 }' "$work/SHA256SUMS")
  [[ -n "$expected" ]] || fail "$name is not listed in the release SHA256SUMS"
  actual=$(sha256 "$work/$name")
  [[ "$actual" == "$expected" ]] || fail "checksum mismatch for $name"
}

install_linux() {
  local version=$1 arch
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
  esac
  local tool
  for tool in tar sha256sum; do
    command -v "$tool" >/dev/null || fail "$tool is required"
  done

  local package="muxflow-$version-linux-$arch"
  printf 'Downloading Muxflow %s for %s...\n' "$version" "$arch"
  fetch_verified "$package.tar.gz" "$version"

  tar -xzf "$work/$package.tar.gz" -C "$work"
  bash "$work/$package/install.sh" || fail "the package installer stopped; nothing was changed"

  local prefix=${ADE_INSTALL_PREFIX:-"$HOME/.local"}

  local missing
  missing=$(ldd "$prefix/lib/muxflow/muxflow" 2>/dev/null | awk '/not found/ { print $1 }' || true)
  if [[ -n "$missing" ]]; then
    printf '\nMissing system libraries:\n%s\n' "$missing" >&2
    printf 'Install GTK 3 and WebKitGTK 4.1 (Arch: webkit2gtk-4.1; Debian/Ubuntu: libwebkit2gtk-4.1-0).\n' >&2
  fi
  command -v tmux >/dev/null \
    || printf '\ntmux 3.3 or newer is required and was not found on PATH.\n' >&2

  case ":$PATH:" in
    *":$prefix/bin:"*) printf '\nRun muxflow, or launch it from your app menu.\n' ;;
    *)
      printf '\n%s/bin is not on your PATH. Add this to your shell profile:\n' "$prefix"
      # shellcheck disable=SC2016 # $PATH is meant literally
      printf '  export PATH="%s/bin:$PATH"\n' "$prefix"
      printf 'Then run muxflow, or launch it from your app menu.\n'
      ;;
  esac
}

install_macos() {
  local version=$1
  [[ "$(uname -m)" == arm64 ]] || fail "the macOS build is for Apple Silicon only"

  # The machine-wide /Applications, as release/macos/install.sh uses: any admin
  # account can write there without sudo.
  local applications=${ADE_MACOS_APPLICATIONS_DIR:-/Applications}
  mkdir -p "$applications" 2>/dev/null || true
  [[ -d "$applications" && -w "$applications" ]] \
    || fail "cannot write to $applications; use an admin account or set ADE_MACOS_APPLICATIONS_DIR=\$HOME/Applications"
  local target="$applications/Muxflow.app"
  if [[ -e "$target" || -L "$target" ]]; then
    [[ -d "$target" && ! -L "$target" ]] && grep -Fxq "$owner" "$target/Contents/Resources/package-owner" 2>/dev/null \
      || fail "$target exists and is not a Muxflow install; move it aside and run again"
  fi

  local dmg="Muxflow_${version}_aarch64.dmg"
  printf 'Downloading Muxflow %s for macOS...\n' "$version"
  fetch_verified "$dmg" "$version"

  mkdir "$work/volume"
  hdiutil attach -quiet -nobrowse -readonly -noautoopen -mountpoint "$work/volume" "$work/$dmg" \
    || fail "could not open $dmg"
  mounted="$work/volume"
  [[ -d "$mounted/Muxflow.app" ]] || fail "$dmg does not contain Muxflow.app"

  # Stage beside the target so the final swap is a rename on one volume, and
  # keep the old copy until the new one is in place.
  local stage
  stage=$(mktemp -d "$applications/.muxflow.install.XXXXXX")
  ditto "$mounted/Muxflow.app" "$stage/new.app" || { rm -rf "$stage"; fail "could not copy Muxflow.app"; }
  hdiutil detach -quiet "$mounted" || true
  mounted=

  if [[ -e "$target" ]] && ! mv "$target" "$stage/old.app"; then
    rm -rf "$stage"
    fail "could not move the previous $target aside; nothing was changed"
  fi
  if ! mv "$stage/new.app" "$target"; then
    [[ ! -d "$stage/old.app" ]] || mv "$stage/old.app" "$target"
    rm -rf "$stage"
    fail "could not install to $target; the previous version was kept"
  fi
  rm -rf "$stage"
  # Published releases are ad-hoc signed, so Gatekeeper refuses a quarantined
  # copy on first launch. The DMG was checked against the release SHA256SUMS
  # above, so clear the flag here instead of sending people to Open Anyway.
  xattr -dr com.apple.quarantine "$target" 2>/dev/null || true

  printf '\nInstalled %s. Open Muxflow from Launchpad or Spotlight; quit and reopen it if it was running.\n' "$target"
  command -v tmux >/dev/null \
    || printf '\ntmux 3.3 or newer is required on every host you attach to. For this Mac: brew install tmux\n' >&2
}

main() {
  local os
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) fail "unsupported OS: $(uname -s)" ;;
  esac

  work=$(mktemp -d)

  local version=${MUXFLOW_VERSION:-}
  if [[ -z "$version" ]]; then
    fetch "$releases/latest/download/latest.json" "$work/latest.json"
    version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$work/latest.json")
  fi
  version=${version#v}
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "invalid version: '$version'"

  "install_$os" "$version"
}

main "$@"

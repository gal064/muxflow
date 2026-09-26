#!/usr/bin/env bash
# One-line Linux installer, published on every release as `install.sh`:
#
#   curl -fsSL https://github.com/gal064/muxflow/releases/latest/download/install.sh | bash
#
# Downloads the release tarball for this machine, checks it against the
# release SHA256SUMS, and runs the package's own install.sh. Running it again
# upgrades in place. MUXFLOW_VERSION pins a release; ADE_INSTALL_PREFIX is
# passed through to the package installer.
set -euo pipefail

releases=${MUXFLOW_RELEASES_URL:-https://github.com/gal064/muxflow/releases}

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

main() {
  case "$(uname -s)" in
    Linux) ;;
    Darwin) fail "this installer is for Linux; on macOS download the DMG from $releases/latest" ;;
    *) fail "unsupported OS: $(uname -s)" ;;
  esac
  local arch
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
  esac
  local tool
  for tool in tar sha256sum; do
    command -v "$tool" >/dev/null || fail "$tool is required"
  done

  local work
  work=$(mktemp -d)
  # shellcheck disable=SC2064 # expand now: $work is local to main
  trap "rm -rf '$work'" EXIT

  local version=${MUXFLOW_VERSION:-}
  if [[ -z "$version" ]]; then
    fetch "$releases/latest/download/latest.json" "$work/latest.json"
    version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$work/latest.json")
  fi
  version=${version#v}
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "invalid version: '$version'"

  local package="muxflow-$version-linux-$arch"
  local base="$releases/download/v$version"
  printf 'Downloading Muxflow %s for %s...\n' "$version" "$arch"
  fetch "$base/$package.tar.gz" "$work/$package.tar.gz"
  fetch "$base/SHA256SUMS" "$work/SHA256SUMS"

  local expected actual
  expected=$(awk -v name="$package.tar.gz" '$2 == name { print $1 }' "$work/SHA256SUMS")
  [[ -n "$expected" ]] || fail "$package.tar.gz is not listed in the release SHA256SUMS"
  actual=$(sha256sum "$work/$package.tar.gz" | cut -d' ' -f1)
  [[ "$actual" == "$expected" ]] || fail "checksum mismatch for $package.tar.gz"

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

main "$@"

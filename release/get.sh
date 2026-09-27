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
  progress_break
  printf 'muxflow: %s\n' "$*" >&2
  exit 1
}

# A single 0-100% bar on stderr, drawn only when stderr is a terminal: under
# `curl | bash` stdin is the pipe but stderr is still the user's terminal, and
# redirected runs (CI, tests) keep plain line output.
progress_on=false
[[ -t 2 && "${TERM:-dumb}" != dumb ]] && progress_on=true
progress_drawn=false
progress_fill='#' progress_empty='-'
case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
  *UTF-8*|*utf-8*|*UTF8*|*utf8*) progress_fill='█' progress_empty='░' ;;
esac

# Draws the bar at percent $1 with label $2.
progress() {
  $progress_on || return 0
  local pct=$1 width=32 filled full empty
  (( pct > 100 )) && pct=100
  filled=$(( pct * width / 100 ))
  printf -v full '%*s' "$filled" ''
  printf -v empty '%*s' $(( width - filled )) ''
  printf '\r\033[K  %s%s %3d%%  %s' "${full// /$progress_fill}" "${empty// /$progress_empty}" "$pct" "$2" >&2
  progress_drawn=true
}

# Ends the bar's line so later output starts on a fresh one.
progress_break() {
  $progress_drawn || return 0
  printf '\n' >&2
  progress_drawn=false
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

# Like fetch, but moves the bar from $3% to $4% as bytes arrive. Falls back to
# a plain fetch without a terminal, without curl, or when the size is unknown.
fetch_progress() {
  local url=$1 dest=$2 from=$3 to=$4 total=0
  if $progress_on && command -v curl >/dev/null; then
    total=$(curl -fsSLI "$url" 2>/dev/null | tr -d '\r' \
      | awk 'tolower($1) == "content-length:" { n = $2 } END { print n + 0 }') || total=0
  fi
  if (( total <= 0 )); then
    fetch "$url" "$dest"
    progress "$to" 'Downloaded'
    return
  fi
  curl -fsSL -o "$dest" "$url" &
  local pid=$! size
  while kill -0 "$pid" 2>/dev/null; do
    size=0
    [[ ! -f "$dest" ]] || size=$(wc -c < "$dest")
    progress $(( from + (to - from) * size / total )) "Downloading $(( size / 1048576 ))/$(( total / 1048576 )) MB"
    sleep 0.2
  done
  wait "$pid" || fail "download failed: $url"
  progress "$to" 'Downloaded'
}

# Downloads release asset $1 of version $2 into $work and checks its checksum,
# moving the bar from $3% to $4% over the download.
fetch_verified() {
  local name=$1 base="$releases/download/v$2"
  fetch_progress "$base/$name" "$work/$name" "$3" "$4"
  progress "$4" 'Verifying checksum'
  fetch "$base/SHA256SUMS" "$work/SHA256SUMS"
  local expected actual
  expected=$(awk -v name="$name" '$2 == name { print $1 }' "$work/SHA256SUMS")
  [[ -n "$expected" ]] || fail "$name is not listed in the release SHA256SUMS"
  actual=$(sha256 "$work/$name")
  [[ "$actual" == "$expected" ]] || fail "checksum mismatch for $name"
}

# Warns, never stops the install: tmux on this machine must be 3.3 or newer,
# and so must tmux on every host Muxflow attaches to. $1 is how to get it here.
# A version tmux reports in a shape this cannot read is left to the app, which
# checks again when it connects.
check_tmux() {
  if ! command -v tmux >/dev/null; then
    printf '\ntmux 3.3 or newer is required and was not found on PATH (%s).\n' "$1" >&2
    return 0
  fi
  local major minor
  read -r major minor < <(tmux -V 2>/dev/null | sed -nE 's/^tmux [^0-9]*([0-9]+)\.([0-9]+).*/\1 \2/p') || true
  [[ -n "${major:-}" && -n "${minor:-}" ]] || return 0
  if (( major < 3 || (major == 3 && minor < 3) )); then
    printf '\ntmux %s.%s is installed; Muxflow needs 3.3 or newer (%s).\n' "$major" "$minor" "$1" >&2
  fi
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
  fetch_verified "$package.tar.gz" "$version" 5 80

  progress 85 'Unpacking'
  tar -xzf "$work/$package.tar.gz" -C "$work"
  progress 92 'Installing'
  # Held back until the bar is finished so its lines do not tear through it.
  local installer_out
  if ! installer_out=$(bash "$work/$package/install.sh" 2>&1); then
    progress_break
    printf '%s\n' "$installer_out" >&2
    fail "the package installer stopped; nothing was changed"
  fi
  progress 100 'Done'
  progress_break
  printf '%s\n' "$installer_out"

  local prefix=${ADE_INSTALL_PREFIX:-"$HOME/.local"}

  case ":$PATH:" in
    *":$prefix/bin:"*) printf '\nRun muxflow, or launch it from your app menu; quit and reopen it if it was running.\n' ;;
    *)
      printf '\n%s/bin is not on your PATH. Add this to your shell profile:\n' "$prefix"
      # shellcheck disable=SC2016 # $PATH is meant literally
      printf '  export PATH="%s/bin:$PATH"\n' "$prefix"
      printf 'Then run muxflow, or launch it from your app menu; quit and reopen it if it was running.\n'
      ;;
  esac
  # Last, so a missing dependency is the final thing on screen rather than
  # scrolled away by the lines above.
  local missing
  missing=$(ldd "$prefix/lib/muxflow/muxflow" 2>/dev/null | awk '/not found/ { print $1 }' || true)
  if [[ -n "$missing" ]]; then
    printf '\nMissing system libraries:\n%s\n' "$missing" >&2
    # WebKitGTK 4.1 pulls in GTK 3, libsoup 3 and JavaScriptCore itself, so one
    # package is the whole fix. The command is printed, never run: this
    # installer stays rootless.
    local fix=
    if command -v apt-get >/dev/null; then fix='sudo apt-get install -y libwebkit2gtk-4.1-0'
    elif command -v dnf >/dev/null; then fix='sudo dnf install -y webkit2gtk4.1'
    elif command -v pacman >/dev/null; then fix='sudo pacman -S --needed webkit2gtk-4.1'
    elif command -v zypper >/dev/null; then fix='sudo zypper install -y libwebkit2gtk-4_1-0'
    fi
    if [[ -n "$fix" ]]; then
      printf 'Install them with:\n  %s\n' "$fix" >&2
    else
      printf 'Install GTK 3 and WebKitGTK 4.1 (Arch: webkit2gtk-4.1; Debian/Ubuntu: libwebkit2gtk-4.1-0).\n' >&2
    fi
  fi
  check_tmux 'install tmux with your package manager'
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
  fetch_verified "$dmg" "$version" 5 75

  progress 80 'Opening disk image'
  mkdir "$work/volume"
  hdiutil attach -quiet -nobrowse -readonly -noautoopen -mountpoint "$work/volume" "$work/$dmg" \
    || fail "could not open $dmg"
  mounted="$work/volume"
  [[ -d "$mounted/Muxflow.app" ]] || fail "$dmg does not contain Muxflow.app"

  # Stage beside the target so the final swap is a rename on one volume, and
  # keep the old copy until the new one is in place.
  local stage
  progress 85 'Copying Muxflow.app'
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
  progress 100 'Done'
  progress_break

  printf '\nInstalled %s. Open Muxflow from Launchpad or Spotlight; quit and reopen it if it was running.\n' "$target"
  # Another copy with the same bundle id (an old download or a local build) can
  # be launched in its place, so name every one Spotlight knows of.
  local other
  while IFS= read -r other; do
    [[ -n "$other" && "$other" != "$target" ]] || continue
    printf 'warning: another Muxflow is at %s and macOS may open it instead; delete it\n' "$other" >&2
  done < <(mdfind 'kMDItemCFBundleIdentifier == "dev.muxflow.desktop"' 2>/dev/null)
  check_tmux 'brew install tmux, or brew upgrade tmux'
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

#!/usr/bin/env bash
# Headless display with software Vulkan for the GPUI spikes. Source it:
#
#   source spikes/gpui/env.sh      # starts everything, exports the variables
#   cargo run -p spike-shared      # a window opens on the private display
#   spike_capture frame.png        # screenshot of the private display
#   spike_env_down                 # stops what was started and removes its files
#
# What it starts, all private to this shell:
#   - Xvfb on a free display number from :100 up (never :0)
#   - a private XDG_RUNTIME_DIR (mode 0700)
#   - a private D-Bus session bus that can only activate org.a11y.Bus, plus an
#     AT-SPI registry on its accessibility bus (AT_SPI_BUS_ADDRESS), so the
#     AccessKit bridge works without touching the owner's session
# WAYLAND_DISPLAY is unset so GPUI picks X11.
#
# Software Vulkan comes from Arch's `vulkan-swrast` (lavapipe), unpacked outside
# the repo into $SPIKE_LAVAPIPE_DIR and used through VK_DRIVER_FILES only; the
# system's Vulkan drivers are not touched. First use: `spike_fetch_lavapipe`.
#
# Options (set before sourcing):
#   SPIKE_LAVAPIPE_DIR     default $HOME/dev/muxflow-gpui-tools/lavapipe
#   SPIKE_SCREEN           default 1280x800x24
#   SPIKE_NO_SYSTEM_FONTS  1 = hide system fonts (private fontconfig, empty dir),
#                          so only embedded fonts can render
#
# This file does not use `set -e`/`set -u`, so sourcing it leaves the caller's
# shell options alone. An EXIT trap is installed only when none exists and the
# shell is not interactive.

_SPIKE_SELF="${BASH_SOURCE[0]}"
SPIKE_LAVAPIPE_DIR="${SPIKE_LAVAPIPE_DIR:-$HOME/dev/muxflow-gpui-tools/lavapipe}"
SPIKE_SCREEN="${SPIKE_SCREEN:-1280x800x24}"

# Pinned version; the package is verified against the local
# pacman sync database (the same SHA-256 pacman itself would check).
_SPIKE_SWRAST_VERSION="1:26.2.5-1"

spike_fetch_lavapipe() {
  local d="$SPIKE_LAVAPIPE_DIR" url want got
  if [ -s "$d/pkg/usr/lib/libvulkan_lvp.so" ]; then
    _spike_write_icd; return
  fi
  mkdir -p "$d/dl" "$d/pkg" || return 1
  url="$(pacman -Sp vulkan-swrast 2>/dev/null | tail -n1)"
  case "$url" in *"vulkan-swrast-$_SPIKE_SWRAST_VERSION"-x86_64.pkg.tar.zst) ;; *)
    echo "spike_fetch_lavapipe: pacman offers '$url', expected vulkan-swrast $_SPIKE_SWRAST_VERSION" >&2
    return 1;; esac
  want="$(for db in /var/lib/pacman/sync/*.db; do
      tar -xOf "$db" --wildcards "vulkan-swrast-$_SPIKE_SWRAST_VERSION/desc" 2>/dev/null
    done | awk '/^%SHA256SUM%$/{getline; print; exit}')"
  [ -n "$want" ] || { echo "spike_fetch_lavapipe: no SHA-256 in the sync database" >&2; return 1; }
  curl -sfL -o "$d/dl/pkg.tar.zst" "$url" || return 1
  got="$(sha256sum "$d/dl/pkg.tar.zst" | cut -d' ' -f1)"
  if [ "$got" != "$want" ]; then
    echo "spike_fetch_lavapipe: SHA-256 mismatch ($got != $want)" >&2
    rm -f "$d/dl/pkg.tar.zst"; return 1
  fi
  # Only the ICD library is used; nothing else from the package is referenced.
  tar --zstd -xf "$d/dl/pkg.tar.zst" -C "$d/pkg" usr/lib/libvulkan_lvp.so usr/share/vulkan/icd.d/lvp_icd.json || return 1
  _spike_write_icd
}

# The packaged ICD JSON names its library relatively; write one with an
# absolute path so the loader needs no LD_LIBRARY_PATH.
_spike_write_icd() {
  local d="$SPIKE_LAVAPIPE_DIR" tmp
  tmp="$(mktemp "$d/lvp_icd.json.XXXXXX")" || return 1
  cat >"$tmp" <<EOF
{
  "file_format_version": "1.0.1",
  "ICD": { "library_path": "$d/pkg/usr/lib/libvulkan_lvp.so", "api_version": "1.4.354" }
}
EOF
  mv -f "$tmp" "$d/lvp_icd.json"
}

spike_env_up() {
  if [ -n "${SPIKE_ENV_PID_XVFB:-}" ]; then
    echo "spike_env_up: already up on DISPLAY=$DISPLAY" >&2; return 0
  fi
  spike_fetch_lavapipe || return 1

  # Remember the caller's values so spike_env_down can put them back.
  local v
  for v in DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET XDG_RUNTIME_DIR XDG_SESSION_TYPE \
           DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS VK_DRIVER_FILES VK_ICD_FILENAMES \
           VK_ADD_DRIVER_FILES FONTCONFIG_FILE; do
    if [ -n "${!v+x}" ]; then export "_SPIKE_SAVED_$v=${!v}"; fi
  done

  SPIKE_ENV_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/muxflow-spike-env.XXXXXXXX")" || return 1
  export XDG_RUNTIME_DIR="$SPIKE_ENV_ROOT/run"
  mkdir -m 0700 "$XDG_RUNTIME_DIR"

  # Pick a display number nobody uses: >= 100, so never the owner's :0, and with
  # no lock file, no socket file and no abstract socket. Xvfb would otherwise
  # replace an existing socket file, so this check must not be skipped. (Xvfb's
  # own -displayfd search was tried first and chose :0 on a machine where
  # Xwayland already owned it.)
  # Parallel spikes source this at the same time. Xvfb's own display lock is racy
  # (two servers can both end up running on one number), so number selection and
  # startup are serialized with flock; success also requires that our Xvfb is
  # alive and its PID is the one in the lock file.
  local n=100 i
  {
    flock -w 60 9 || { echo "spike_env_up: display lock timeout" >&2; spike_env_down; return 1; }
    while :; do
      while [ -e "/tmp/.X$n-lock" ] || [ -e "/tmp/.X11-unix/X$n" ] \
            || grep -q "/tmp/.X11-unix/X$n\$" /proc/net/unix; do
        n=$((n + 1))
      done
      if [ $n -gt 199 ]; then echo "spike_env_up: no free display number" >&2; spike_env_down; return 1; fi
      Xvfb ":$n" -screen 0 "$SPIKE_SCREEN" -nolisten tcp -noreset \
        9>&- >/dev/null </dev/null 2>"$SPIKE_ENV_ROOT/xvfb.log" &
      SPIKE_ENV_PID_XVFB=$!
      i=0
      while [ ! -S "/tmp/.X11-unix/X$n" ] && [ $i -lt 100 ]; do
        kill -0 "$SPIKE_ENV_PID_XVFB" 2>/dev/null || break
        sleep 0.1; i=$((i + 1))
      done
      if kill -0 "$SPIKE_ENV_PID_XVFB" 2>/dev/null && [ -S "/tmp/.X11-unix/X$n" ] \
         && [ "$(tr -d ' \n' <"/tmp/.X$n-lock" 2>/dev/null)" = "$SPIKE_ENV_PID_XVFB" ]; then
        break
      fi
      i=0   # a losing Xvfb takes a moment to notice and exit
      while kill -0 "$SPIKE_ENV_PID_XVFB" 2>/dev/null && [ $i -lt 30 ]; do sleep 0.1; i=$((i + 1)); done
      if kill -0 "$SPIKE_ENV_PID_XVFB" 2>/dev/null; then
        echo "spike_env_up: Xvfb did not start; see $SPIKE_ENV_ROOT/xvfb.log" >&2
        cat "$SPIKE_ENV_ROOT/xvfb.log" >&2
        spike_env_down; return 1
      fi
      wait "$SPIKE_ENV_PID_XVFB" 2>/dev/null || true
      if ! grep -q "already active\|already in use" "$SPIKE_ENV_ROOT/xvfb.log"; then
        echo "spike_env_up: Xvfb failed:" >&2; cat "$SPIKE_ENV_ROOT/xvfb.log" >&2
        spike_env_down; return 1
      fi
      n=$((n + 1))   # lost the race for this number; try the next
    done
  } 9>/tmp/muxflow-spike-display.lock
  echo "$n" >"$SPIKE_ENV_ROOT/display"
  export DISPLAY=":$(tr -d '\n' <"$SPIKE_ENV_ROOT/display")"
  unset WAYLAND_DISPLAY WAYLAND_SOCKET
  export XDG_SESSION_TYPE=x11

  # Private session bus; only the AT-SPI bus launcher is activatable on it.
  mkdir "$SPIKE_ENV_ROOT/services"
  ln -s /usr/share/dbus-1/services/org.a11y.Bus.service "$SPIKE_ENV_ROOT/services/"
  cat >"$SPIKE_ENV_ROOT/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:path=$XDG_RUNTIME_DIR/bus</listen>
  <auth>EXTERNAL</auth>
  <servicedir>$SPIKE_ENV_ROOT/services</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
  dbus-daemon --config-file="$SPIKE_ENV_ROOT/bus.conf" --nofork --nopidfile \
    >/dev/null </dev/null 2>"$SPIKE_ENV_ROOT/dbus.log" &
  SPIKE_ENV_PID_DBUS=$!
  i=0
  while [ ! -S "$XDG_RUNTIME_DIR/bus" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
  export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
  unset AT_SPI_BUS_ADDRESS

  # AT-SPI: this activates the a11y bus (at-spi-bus-launcher, via the private
  # bus), then runs the registry on it. The packaged D-Bus activation of the
  # registry passes --use-gnome-session and fails without a GNOME session.
  local a11y
  a11y="$(dbus-send --session --print-reply=literal --dest=org.a11y.Bus \
            /org/a11y/bus org.a11y.Bus.GetAddress 2>/dev/null | tr -d ' \t\n')" || true
  if [ -n "$a11y" ]; then
    export AT_SPI_BUS_ADDRESS="$a11y"
    DBUS_SESSION_BUS_ADDRESS="$a11y" /usr/lib/at-spi2-registryd \
      >>"$SPIKE_ENV_ROOT/registryd.log" </dev/null 2>&1 &
    SPIKE_ENV_PID_REGISTRY=$!
    # accesskit_unix only publishes a tree once the bus says assistive tech is
    # on (a screen reader would set this). Set it, so S10 does not depend on it.
    dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
      org.freedesktop.DBus.Properties.Set string:org.a11y.Status string:IsEnabled \
      variant:boolean:true >/dev/null 2>&1 || true
  else
    echo "spike_env_up: could not start the AT-SPI bus; accessibility tree unavailable" >&2
  fi

  # Software Vulkan only: this ICD file and nothing else.
  export VK_DRIVER_FILES="$SPIKE_LAVAPIPE_DIR/lvp_icd.json"
  unset VK_ICD_FILENAMES VK_ADD_DRIVER_FILES

  if [ "${SPIKE_NO_SYSTEM_FONTS:-}" = 1 ]; then
    mkdir "$SPIKE_ENV_ROOT/nofonts"
    cat >"$SPIKE_ENV_ROOT/fonts.conf" <<EOF
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "fonts.dtd">
<fontconfig><dir>$SPIKE_ENV_ROOT/nofonts</dir><cachedir>$SPIKE_ENV_ROOT/fccache</cachedir></fontconfig>
EOF
    export FONTCONFIG_FILE="$SPIKE_ENV_ROOT/fonts.conf"
  fi

  if [ -z "$(trap -p EXIT)" ] && [[ $- != *i* ]]; then
    trap spike_env_down EXIT
    _SPIKE_TRAP_SET=1
  fi
  echo "spike env up: DISPLAY=$DISPLAY XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR VK_DRIVER_FILES=$VK_DRIVER_FILES"
}

# Screenshot of the whole private display. Usage: spike_capture out.png
spike_capture() {
  [ -n "${1:-}" ] || { echo "usage: spike_capture out.png" >&2; return 2; }
  import -display "$DISPLAY" -window root "$1"
}

spike_env_down() {
  local p
  for p in "${SPIKE_ENV_PID_REGISTRY:-}" "${SPIKE_ENV_PID_DBUS:-}" "${SPIKE_ENV_PID_XVFB:-}"; do
    [ -n "$p" ] && { kill "$p" 2>/dev/null || true; }
  done
  # The AT-SPI bus launcher, the a11y bus, and any app started in this
  # environment inherited the private XDG_RUNTIME_DIR; stop exactly those.
  # Keyed on our own directory, never on the live variable, which a caller may
  # have changed (the owner's /run/user/<uid> is carried by their whole session).
  if [ -n "${SPIKE_ENV_ROOT:-}" ] && [ -d "$SPIKE_ENV_ROOT/run" ]; then
    local pid
    for pid in $(pgrep -u "$(id -u)" '.' 2>/dev/null); do
      [ "$pid" = "$$" ] || [ "$pid" = "$PPID" ] && continue
      [ -r "/proc/$pid/environ" ] || continue
      if { tr '\0' '\n' <"/proc/$pid/environ"; } 2>/dev/null | grep -qxF "XDG_RUNTIME_DIR=$SPIKE_ENV_ROOT/run"; then
        kill "$pid" 2>/dev/null || true
      fi
    done
  fi
  [ -n "${SPIKE_ENV_PID_XVFB:-}" ] && { wait "$SPIKE_ENV_PID_XVFB" 2>/dev/null || true; }
  [ -n "${SPIKE_ENV_PID_DBUS:-}" ] && { wait "$SPIKE_ENV_PID_DBUS" 2>/dev/null || true; }
  [ -n "${SPIKE_ENV_ROOT:-}" ] && case "$SPIKE_ENV_ROOT" in */muxflow-spike-env.*) rm -rf "$SPIKE_ENV_ROOT";; esac
  unset SPIKE_ENV_PID_XVFB SPIKE_ENV_PID_DBUS SPIKE_ENV_PID_REGISTRY SPIKE_ENV_ROOT
  local v saved
  for v in DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET XDG_RUNTIME_DIR XDG_SESSION_TYPE \
           DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS VK_DRIVER_FILES VK_ICD_FILENAMES \
           VK_ADD_DRIVER_FILES FONTCONFIG_FILE; do
    saved="_SPIKE_SAVED_$v"
    if [ -n "${!saved+x}" ]; then export "$v=${!saved}"; else unset "$v"; fi
    unset "$saved"
  done
  [ -n "${_SPIKE_TRAP_SET:-}" ] && { trap - EXIT; unset _SPIKE_TRAP_SET; }
  return 0
}

# Sourcing starts the environment; running the file directly only explains.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  echo "Source this file: source $_SPIKE_SELF" >&2
  exit 2
fi
spike_env_up

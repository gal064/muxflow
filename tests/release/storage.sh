#!/usr/bin/env bash
# Disk-bounded storage policy shared by the heavy Phase 8 verification gates.
# Call phase8_storage_begin once, add gate-specific cleanup to the caller's
# EXIT trap, then call phase8_storage_publish after all assertions pass.

# Portable shims. These gates run both on the Linux remote targets and on the
# Darwin release host, and coreutils/util-linux options are not shared between
# them. Each shim reproduces the exact semantics the original GNU invocation
# asserted, so a Darwin run tests the same property as a Linux run.

phase8_is_darwin() { [[ $(uname -s) == Darwin ]]; }

# Allocated size of a tree in bytes. GNU: du -s -B1
phase8_dir_bytes() {
  if phase8_is_darwin; then
    du -sk "$1" 2>/dev/null | awk '{print $1 * 1024}'
  else
    du -s -B1 "$1" 2>/dev/null | awk '{print $1}'
  fi
}

# Immediate subdirectories matching a glob, newest mtime first.
# GNU: find -printf '%T@ %p\n'
phase8_dirs_newest_first() {
  local root=$1 pattern=$2
  if phase8_is_darwin; then
    find "$root" -mindepth 1 -maxdepth 1 -type d -name "$pattern" \
      -exec stat -f '%m %N' {} + 2>/dev/null | sort -rn | cut -d' ' -f2-
  else
    find "$root" -mindepth 1 -maxdepth 1 -type d -name "$pattern" \
      -printf '%T@ %p\n' 2>/dev/null | sort -rn | cut -d' ' -f2-
  fi
}

# Path of $2 relative to absolute base $1. GNU: realpath --relative-to=
# Falls back to the absolute path when $2 is not under $1; the symlink the
# caller creates stays valid either way.
phase8_relative_to() {
  local base=${1%/} target=$2
  if [[ "$target" == "$base"/* ]]; then
    printf '%s\n' "${target#"$base"/}"
  else
    printf '%s\n' "$target"
  fi
}

# File metadata. GNU: stat -c '%a' / '%s' / '%u'
phase8_stat_mode() { if phase8_is_darwin; then stat -f '%Lp' "$1"; else stat -c '%a' "$1"; fi; }
phase8_stat_size() { if phase8_is_darwin; then stat -f '%z' "$1"; else stat -c '%s' "$1"; fi; }
phase8_stat_uid() { if phase8_is_darwin; then stat -f '%u' "$1"; else stat -c '%u' "$1"; fi; }

# Evidence detail line. GNU: stat -c 'path=%n mode=%a uid=%u size=%s blocks=%b'
phase8_stat_detail() {
  if phase8_is_darwin; then
    stat -f 'path=%N mode=%Lp uid=%u size=%z blocks=%b' "$@"
  else
    stat -c 'path=%n mode=%a uid=%u size=%s blocks=%b' "$@"
  fi
}

# Is a TCP port already listening? iproute2: ss -H -ltn "sport = :PORT"
# A missing ss previously made the caller's emptiness test succeed for every
# candidate, so the first port was chosen without ever checking availability.
phase8_tcp_port_listening() {
  if command -v ss >/dev/null 2>&1; then
    [[ -n "$(ss -H -ltn "sport = :$1")" ]]
  else
    lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
  fi
}

# Emit any listening TCP sockets owned by a pid; non-zero exit when there are
# none. iproute2: ss -H -ltnp | grep -F "pid=PID,". This backs a security
# assertion that the daemon opens no TCP listener, so a missing ss must not be
# allowed to satisfy it vacuously.
phase8_pid_tcp_listeners() {
  if command -v ss >/dev/null 2>&1; then
    ss -H -ltnp 2>/dev/null | grep -F "pid=$1,"
  else
    lsof -nP -a -p "$1" -iTCP -sTCP:LISTEN 2>/dev/null | grep -v '^COMMAND'
  fi
}

# Resident-memory evidence for a pid. Linux reports the kernel's exact peak
# (VmHWM). Darwin exposes no peak RSS for another process through any shipped
# command line tool, so it records the current resident size and labels it as
# such rather than presenting a different metric under the VmHWM name. This
# artifact is evidence only; no gate asserts against it.
phase8_pid_memory_evidence() {
  local pid=$1 rss
  if phase8_is_darwin; then
    rss=$(ps -o rss= -p "$pid" | tr -d ' ')
    printf 'RssCurrent:\t%s kB\t(Darwin: current resident size; peak RSS is not exposed for another process)\n' \
      "${rss:-unavailable}"
  else
    sed -n '/^VmHWM:/p' "/proc/$pid/status"
  fi
}

# An isolated DOCKER_CONFIG keeps a gate run away from the developer's registry
# credentials, but it also hides the Docker CLI plugin directory, and buildx
# lives there. Without buildx the CLI falls back to the legacy builder, which
# reuses a cached layer built for a different platform and then fails to export
# it, so a --platform build silently depends on whichever architecture populated
# the cache first. Link the plugins back in and keep BuildKit on.
phase8_isolate_docker_config() {
  local directory=$1
  mkdir -p "$directory"
  chmod 0700 "$directory"
  local plugins="$HOME/.docker/cli-plugins"
  if [[ -d "$plugins" && ! -e "$directory/cli-plugins" ]]; then
    ln -s "$plugins" "$directory/cli-plugins"
  fi
  export DOCKER_BUILDKIT=1
  export DOCKER_CONFIG="$directory"
}

# Linux target architecture for the Docker-backed remote gates. An emulated
# container is not equivalent to a native one for these lanes: a Rosetta-
# translated process reports /proc/<pid>/exe as the translator rather than the
# real binary, which silently defeats the helper's process-identity check
# (M10-E037). Default to this machine's own architecture so nothing is
# translated; an explicit override still allows a cross-architecture run on a
# runner that can execute it natively.
phase8_linux_target_arch() {
  local arch=${ADE_LINUX_TARGET_ARCH:-$(uname -m)}
  case "$arch" in
    x86_64 | amd64) printf 'x86_64\n' ;;
    aarch64 | arm64) printf 'aarch64\n' ;;
    *)
      echo "unsupported Linux target architecture: $arch" >&2
      return 64
      ;;
  esac
}

phase8_docker_platform() {
  local arch
  arch=$(phase8_linux_target_arch) || return $?
  case "$arch" in
    x86_64) printf 'linux/amd64\n' ;;
    aarch64) printf 'linux/arm64\n' ;;
  esac
}

# GNU: timeout --foreground --kill-after=<kill>s <limit>s CMD...
# macOS ships no timeout(1). Reproduce it with TERM-then-KILL escalation and
# the same exit code 124 on expiry.
phase8_timeout() {
  local limit=$1 kill_after=$2
  shift 2
  if command -v timeout >/dev/null 2>&1; then
    timeout --foreground --kill-after="${kill_after}s" "${limit}s" "$@"
    return $?
  fi
  "$@" &
  local pid=$! waited=0
  while ((waited < limit * 10)); do
    kill -0 "$pid" 2>/dev/null || { wait "$pid"; return $?; }
    sleep 0.1
    ((waited += 1))
  done
  kill -TERM "$pid" 2>/dev/null || true
  waited=0
  while ((waited < kill_after * 10)); do
    kill -0 "$pid" 2>/dev/null || { wait "$pid" 2>/dev/null; return 124; }
    sleep 0.1
    ((waited += 1))
  done
  kill -KILL "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  return 124
}

phase8_storage_begin() {
  local repo_root=$1
  local gate_name=$2
  local minimum_free_kib=${ADE_PHASE8_MIN_FREE_KIB:-26214400}
  local filesystem available_kib mount_point

  export ADE_WORK_ROOT="${ADE_WORK_ROOT:-$repo_root/tmp/work}"
  export ADE_EVIDENCE_ROOT="${ADE_EVIDENCE_ROOT:-$repo_root/tmp/evidence}"
  mkdir -p "$ADE_WORK_ROOT/runs" "$ADE_WORK_ROOT/t" \
    "$ADE_WORK_ROOT/cache/cargo-target" "$ADE_EVIDENCE_ROOT"

  # findmnt is util-linux only. Darwin resolves the containing mount point with
  # df, then reads that mount's type from mount(8). Note that macOS reports an
  # hdiutil RAM disk as apfs/hfs, so this rejects the Linux memory-backed types
  # but cannot detect a deliberately constructed macOS RAM disk.
  if [[ $(uname -s) == Darwin ]]; then
    mount_point=$(df -P "$ADE_WORK_ROOT" \
      | awk 'NR == 2 {for (i = 6; i <= NF; i++) printf "%s%s", $i, (i < NF ? " " : "")}')
    filesystem=$(mount | sed -n "s|^.* on ${mount_point} (\([^,)]*\).*|\1|p" | head -n 1)
    [[ -n "$filesystem" ]]
  else
    filesystem=$(findmnt -n -o FSTYPE -T "$ADE_WORK_ROOT")
  fi
  case "$filesystem" in
    tmpfs|ramfs)
      echo "Phase 8 work root must be disk-backed, got $filesystem: $ADE_WORK_ROOT" >&2
      return 78
      ;;
  esac
  available_kib=$(df -Pk "$ADE_WORK_ROOT" | awk 'NR == 2 {print $4}')
  if ((available_kib < minimum_free_kib)); then
    echo "Phase 8 needs at least $minimum_free_kib KiB free at $ADE_WORK_ROOT; found $available_kib KiB" >&2
    return 78
  fi

  # Keep ordinary process temporary files on a real (non-symlink) path. Rust
  # path-confinement tests intentionally compare lexical and canonical roots,
  # so routing TMPDIR through a convenience symlink would invalidate those
  # security assertions. This direct path is also short enough for the
  # desktop's hashed OpenSSH control socket (< 108 bytes).
  export TMPDIR="$ADE_WORK_ROOT/t"
  export CARGO_INCREMENTAL=0
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ADE_WORK_ROOT/cache/cargo-target}"
  PHASE8_GATE_NAME=$gate_name
  PHASE8_WORK_DIR=$(mktemp -d "$ADE_WORK_ROOT/runs/$gate_name.XXXXXX")
  PHASE8_EVIDENCE_DIR="$ADE_EVIDENCE_ROOT/$gate_name.$(date -u +%Y%m%dT%H%M%SZ)-$$"
  mkdir -p "$PHASE8_EVIDENCE_DIR"
  PHASE8_STORAGE_PUBLISHED=0
  PHASE8_STORAGE_MONITOR_PID=''
  printf '%s\n' "$filesystem" >"$PHASE8_EVIDENCE_DIR/work-filesystem.txt"
  df -Pk "$ADE_WORK_ROOT" >"$PHASE8_EVIDENCE_DIR/disk-before.txt"
  (
    peak=0
    while [[ -d "$PHASE8_WORK_DIR" ]]; do
      bytes=$(phase8_dir_bytes "$PHASE8_WORK_DIR")
      [[ "$bytes" =~ ^[0-9]+$ ]] || bytes=0
      ((bytes <= peak)) || peak=$bytes
      printf '%s\n' "$peak" >"$PHASE8_EVIDENCE_DIR/peak-work-bytes.txt"
      sleep 2
    done
  ) &
  PHASE8_STORAGE_MONITOR_PID=$!
}

phase8_storage_publish() {
  local repo_root=$1
  local latest_name=$2
  local relative
  relative=$(phase8_relative_to "$repo_root/tmp" "$PHASE8_EVIDENCE_DIR")
  ln -sfn "$relative" "$repo_root/tmp/$latest_name"
  PHASE8_STORAGE_PUBLISHED=1
}

phase8_storage_prune() {
  local keep=${ADE_PHASE8_EVIDENCE_KEEP:-3}
  local count=0
  local directory
  local -a candidates=()
  while IFS= read -r directory; do candidates+=("$directory"); done < <(
    phase8_dirs_newest_first "$ADE_EVIDENCE_ROOT" "$PHASE8_GATE_NAME.*"
  )
  for directory in "${candidates[@]}"; do
    ((count += 1))
    ((count <= keep)) && continue
    [[ "$directory" == "$ADE_EVIDENCE_ROOT/$PHASE8_GATE_NAME."* ]] || continue
    rm -rf -- "$directory"
  done
}

phase8_storage_finish() {
  local exit_code=$1
  local peak=0 retained=0
  if [[ -n "${PHASE8_STORAGE_MONITOR_PID:-}" ]]; then
    kill "$PHASE8_STORAGE_MONITOR_PID" >/dev/null 2>&1 || true
    wait "$PHASE8_STORAGE_MONITOR_PID" 2>/dev/null || true
  fi
  [[ ! -f "$PHASE8_EVIDENCE_DIR/peak-work-bytes.txt" ]] || \
    peak=$(<"$PHASE8_EVIDENCE_DIR/peak-work-bytes.txt")
  if ((exit_code != 0)); then
    # Preserve only compact diagnostic artifacts. Never retain payloads,
    # private keys, checkouts, package trees, or Cargo targets from a failure.
    find "$PHASE8_WORK_DIR" -mindepth 1 -maxdepth 1 -type f -size -5M \
      \( -name '*.log' -o -name '*.json' -o -name '*.txt' -o -name '*.tsv' \
         -o -name '*.out' -o -name '*.err' \) \
      -exec cp -- {} "$PHASE8_EVIDENCE_DIR/" \; 2>/dev/null || true
  fi
  rm -rf -- "$PHASE8_WORK_DIR"
  df -Pk "$ADE_WORK_ROOT" >"$PHASE8_EVIDENCE_DIR/disk-after.txt"
  retained=$(phase8_dir_bytes "$PHASE8_EVIDENCE_DIR")
  jq -n --arg status "$([[ $exit_code == 0 && $PHASE8_STORAGE_PUBLISHED == 1 ]] && printf pass || printf fail)" \
    --argjson peakWorkBytes "${peak:-0}" --argjson retainedEvidenceBytes "${retained:-0}" \
    '{status:$status, peakWorkBytes:$peakWorkBytes, retainedEvidenceBytes:$retainedEvidenceBytes,
      workRootDiskBacked:true, cargoIncremental:false}' \
    >"$PHASE8_EVIDENCE_DIR/storage.json"
  phase8_storage_prune
}

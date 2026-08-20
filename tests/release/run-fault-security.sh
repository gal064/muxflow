#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo_root"
source tests/release/storage.sh
phase8_storage_begin "$repo_root" phase8-fault-security
run_root=$PHASE8_WORK_DIR
evidence=$PHASE8_EVIDENCE_DIR
host_binary="$CARGO_TARGET_DIR/debug/muxflow-host"
fixture_home="$run_root/home"
runtime="$run_root/runtime"
mkdir -p "$fixture_home" "$runtime"

cleanup() {
  local status=$?
  HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$host_binary" daemon-stop >/dev/null 2>&1 || true
  phase8_storage_finish "$status"
}
trap cleanup EXIT

cargo test -p muxflow-host diagnostics -- --nocapture > "$run_root/diagnostics-tests.log" 2>&1
cargo test -p muxflow-host symlink -- --nocapture > "$run_root/host-symlink-tests.log" 2>&1
cargo test -p tmux-control malformed -- --nocapture > "$run_root/parser-malformed-tests.log" 2>&1
cargo test -p muxflow reused_id -- --nocapture > "$run_root/notification-id-tests.log" 2>&1
cargo test -p muxflow schema -- --nocapture > "$run_root/schema-tests.log" 2>&1
cargo run --manifest-path tests/integration/fuzz-smoke/Cargo.toml -- 10000 > "$run_root/fuzz-smoke.log" 2>&1

cargo build -p muxflow-host > "$run_root/host-build.log" 2>&1
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$host_binary" doctor --json \
  > "$run_root/doctor.json"
bundle="$run_root/support-bundle.json"
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$host_binary" support-bundle --output "$bundle"
[[ "$(phase8_stat_mode "$bundle")" == "600" ]]
jq -e '.privacy.telemetryUploaded == false and .privacy.terminalOutputIncluded == false and .privacy.promptTextIncluded == false and .privacy.fileContentIncluded == false and .privacy.credentialsIncluded == false and .privacy.pathsOrHostnamesIncluded == false' "$bundle" >/dev/null
if rg -i '(BEGIN.*PRIVATE KEY|bearer [a-z0-9]|password=|api[_-]?key=|/home/operator|phase8-secret)' "$bundle" > "$run_root/bundle-secret-scan.log"; then
  echo "support bundle contains a forbidden secret/content marker" >&2
  exit 1
fi
if HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$host_binary" support-bundle --output "$bundle" \
  > "$run_root/bundle-overwrite.out" 2> "$run_root/bundle-overwrite.err"; then
  echo "support bundle unexpectedly overwrote an existing destination" >&2
  exit 1
fi
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$host_binary" daemon \
  > "$run_root/daemon.log" 2>&1 &
daemon_launcher=$!
for _ in $(seq 1 100); do
  [[ -f "$runtime/daemon.json" ]] && break
  sleep 0.05
done
[[ -f "$runtime/daemon.json" ]]
daemon_pid=$(jq -r '.pid' "$runtime/daemon.json")
if phase8_pid_tcp_listeners "$daemon_pid" > "$run_root/unexpected-tcp-listener.log"; then
  echo "muxflow-host unexpectedly opened a TCP listener" >&2
  exit 1
fi
printf 'no TCP listener for daemon pid %s\n' "$daemon_pid" > "$run_root/no-tcp-listener.log"
HOME="$fixture_home" ADE_HOST_RUNTIME_DIR="$runtime" "$host_binary" daemon-stop
wait "$daemon_launcher" || true

agent_probe_failures=0
: > "$run_root/agent-probes.tsv"
for agent in codex claude; do
  if command -v "$agent" >/dev/null 2>&1; then
    for probe in version help; do
      if phase8_timeout 10 2 "$agent" "--$probe" </dev/null \
        > "$run_root/$agent-$probe.log" 2>&1; then
        printf '%s\t%s\tpass\n' "$agent" "$probe" >> "$run_root/agent-probes.tsv"
      else
        status=$?
        agent_probe_failures=$((agent_probe_failures + 1))
        printf '%s\t%s\tlimited-exit-%s\n' "$agent" "$probe" "$status" \
          >> "$run_root/agent-probes.tsv"
      fi
    done
  else
    printf '%s is not installed; fixture adapters cover behavior\n' "$agent" > "$run_root/$agent-not-installed.log"
    printf '%s\tavailability\tnot-installed\n' "$agent" >> "$run_root/agent-probes.tsv"
  fi
done

jq -n --argjson agentProbeFailures "$agent_probe_failures" \
  '{status:"pass", diagnosticsRedaction:true, parserFuzzCases:10000, symlinkAndSchemaGuards:true, notificationIdReuse:true, noTcpListener:true, installedAgentCommands:"version/help only", agentProbeFailures:$agentProbeFailures}' \
  > "$run_root/result.json"
find "$run_root" -mindepth 1 -maxdepth 1 -type f -exec cp {} "$evidence/" \;
phase8_storage_publish "$repo_root" phase8-fault-security-latest
cat "$run_root/result.json"

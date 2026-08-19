import json
import platform
import sys
from pathlib import Path


REQUIRED_WIRE_METRICS = {
    "keystroke -> glyph echo p95",
    "window switch (visible+interactive) p95",
    "new tab -> pane interactive p95",
    "new workspace -> pane interactive p95",
    "resize settle p95",
    "sustained output throughput",
    "idle host frames in 8 s",
    "connection-wide resyncs across the run",
    "event sequence gaps",
    "app overhead vs raw ssh+tmux p95",
}


def lane_verdict(budget: str, lane: str, artifact_exists: bool, flood_seconds: int) -> str:
    if not artifact_exists:
        return "FAIL"
    rows = [line for line in budget.splitlines() if len(line.split(None, 2)) == 3 and line.split(None, 2)[1] == lane]
    required_metrics = REQUIRED_WIRE_METRICS | {
        f"connection resyncs during {flood_seconds} s flood",
    }
    required_rows = {
        metric: [row for row in rows if row.split(None, 2)[2].startswith(metric)]
        for metric in required_metrics
    }
    if any(not values for values in required_rows.values()):
        return "FAIL"
    if any(row.startswith(("FAIL", "MISS")) for values in required_rows.values() for row in values):
        return "FAIL"
    return "PASS"


def main() -> int:
    if len(sys.argv) != 6:
        return 2
    root = Path(sys.argv[1])
    deterministic_status = int(sys.argv[2])
    phase12_status = int(sys.argv[3])
    phase12_evidence_status = int(sys.argv[4])
    flood_seconds = int(sys.argv[5])
    docker_status_path = root / "phase12" / "docker-status.txt"
    docker_status = docker_status_path.read_text(encoding="utf-8").strip() if docker_status_path.exists() else "not-run"
    budget = (root / "phase12" / "budget-report.txt").read_text(encoding="utf-8", errors="replace") if (root / "phase12" / "budget-report.txt").exists() else ""
    if phase12_status != 0 or phase12_evidence_status != 0:
        docker_lane_status = "FAIL"
        local_lane_status = "FAIL"
    else:
        docker_lane_status = (
            "BLOCKED"
            if docker_status != "ran"
            else lane_verdict(
                budget,
                "docker",
                (root / "phase12" / "docker-app.json").exists(),
                flood_seconds,
            )
        )
        local_lane_status = lane_verdict(
            budget,
            "local",
            (root / "phase12" / "local-app.json").exists(),
            flood_seconds,
        )
    macos_runner = platform.system() == "Darwin"
    packaged_status = "NOT_RUN" if macos_runner else "BLOCKED"
    packaged_coverage = "NOT_CAPTURED" if macos_runner else "PLATFORM_BLOCKED"
    packaged_detail = (
        "run the separate packaged-app journey on this macOS host"
        if macos_runner
        else "requires a macOS packaged-app runner"
    )
    deterministic_pass = deterministic_status == 0
    workspace_captured = docker_lane_status == "PASS"
    explorer_captured = deterministic_pass and (root / "logs" / "explorer-wide.log").is_file()
    git_captured = deterministic_pass and (root / "logs" / "git-consumers.log").is_file()
    lanes = [
        {"name": "deterministic-operation-fixtures", "status": "PASS" if deterministic_status == 0 else "FAIL"},
        {"name": "shaped-100ms-ssh-docker", "status": docker_lane_status, "detail": docker_status},
        {"name": "local-wire-control", "status": local_lane_status, "detail": "host/link captured; whole-desktop idle polling remains unmeasured" if local_lane_status == "BLOCKED" else "captured"},
        {"name": "macos-packaged-cua", "status": packaged_status, "detail": packaged_detail},
        {"name": "visible-background-idle-cpu-rss", "status": "BLOCKED", "detail": "requires a launchable native desktop and display"},
        {"name": "stalled-webview-rss", "status": "BLOCKED", "detail": "requires a launchable native desktop/WebView stall controller"},
    ]
    coverage = [
        {
            "journey": "connect/control-master establishment and reuse",
            "status": packaged_coverage,
            "detail": f"instrumented counters are available; {packaged_detail}",
        },
        {
            "journey": "workspace/tab create-open-select",
            "status": "PARTIAL" if workspace_captured else "NOT_CAPTURED",
            "detail": (
                "passing shaped SSH create-session/create-window ack and interactive spans captured; packaged target spans and exact tmux process/round-trip counters require the macOS lane"
                if workspace_captured
                else f"shaped SSH lane was {docker_lane_status.lower()}; no remote workflow capture is claimed"
            ),
        },
        {
            "journey": "file open/first content/cancellation",
            "status": packaged_coverage,
            "detail": "request, byte, first-content, and cancellation hooks are instrumented; no packaged WebView is available",
        },
        {
            "journey": "Explorer list/watch/expand",
            "status": "PARTIAL" if explorer_captured else "NOT_CAPTURED",
            "detail": (
                "4,096-entry deterministic list/watch/DOM counters captured by a passing fixture; remote stat/list latency, payload, and cache observations require the packaged SSH lane"
                if explorer_captured
                else "the deterministic lane or Explorer artifact did not pass; no Explorer capture is claimed"
            ),
        },
        {
            "journey": "Git status/diff/mutation",
            "status": "PARTIAL" if git_captured else "NOT_CAPTURED",
            "detail": (
                "32-consumer watcher/status-process fixture captured by a passing deterministic lane; remote action bytes, cancellation, and perceived spans require the packaged SSH lane"
                if git_captured
                else "the deterministic lane or Git artifact did not pass; no Git capture is claimed"
            ),
        },
        {
            "journey": "startup/font/Monaco milestones",
            "status": packaged_coverage,
            "detail": "milestones are instrumented; native packaged launch is unavailable",
        },
        {
            "journey": "Tauri bridge stalled-consumer bytes/RSS",
            "status": packaged_coverage,
            "detail": "ingress/send/admission-lag/retained-byte hooks are instrumented; native WebView stall control is unavailable",
        },
    ]
    summary = {
        "schemaVersion": 1,
        "label": root.name,
        "phase12CommandExit": phase12_status,
        "phase12EvidenceValidationExit": phase12_evidence_status,
        "phase12FloodSeconds": flood_seconds,
        "lanes": lanes,
        "measurementCoverage": coverage,
    }
    (root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    required_failed = any(lane["status"] != "PASS" for lane in lanes[:3])
    return 1 if required_failed else 0


raise SystemExit(main())

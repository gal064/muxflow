import json
import hashlib
import os
import platform
import subprocess
import sys
from pathlib import Path


def command(*arguments: str) -> dict[str, object]:
    try:
        result = subprocess.run(arguments, text=True, capture_output=True, timeout=20)
        return {
            "command": list(arguments),
            "exitCode": result.returncode,
            "stdout": result.stdout.strip(),
            "stderr": result.stderr.strip(),
        }
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"command": list(arguments), "error": str(error)}


def source_digest() -> dict[str, object]:
    listed = subprocess.run(
        ("git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"),
        capture_output=True,
        check=True,
    ).stdout.split(b"\0")
    digest = hashlib.sha256()
    count = 0
    for raw_path in sorted(path for path in listed if path):
        path = Path(raw_path.decode("utf-8", errors="surrogateescape"))
        if path.parts[0] in {"node_modules", "target", "tmp"} or "node_modules" in path.parts:
            continue
        digest.update(raw_path)
        digest.update(b"\0")
        if path.is_symlink():
            digest.update(os.readlink(path).encode("utf-8", errors="surrogateescape"))
        else:
            digest.update(path.read_bytes())
        digest.update(b"\0")
        count += 1
    return {"algorithm": "sha256", "digest": digest.hexdigest(), "fileCount": count}


def main() -> int:
    if len(sys.argv) not in {2, 3}:
        return 2
    output = Path(sys.argv[1])
    context = {
        "schemaVersion": 1,
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": sys.version,
        "commit": command("git", "rev-parse", "HEAD"),
        "dirty": command("git", "status", "--porcelain"),
        "sourceTree": source_digest(),
        "toolchains": {
            "rustc": command("rustc", "--version"),
            "cargo": command("cargo", "--version"),
            "node": command("node", "--version"),
            "pnpm": command("pnpm", "--version"),
            "uv": command("uv", "--version"),
            "tmux": command("tmux", "-V"),
            "docker": command("docker", "version", "--format", "{{.Client.Version}}"),
        },
        "display": {
            "DISPLAY": bool(os.environ.get("DISPLAY")),
            "WAYLAND_DISPLAY": bool(os.environ.get("WAYLAND_DISPLAY")),
            "XDG_SESSION_TYPE": os.environ.get("XDG_SESSION_TYPE", ""),
        },
        "primaryDeployment": "macOS desktop to remote Linux over SSH",
        "hostRunnerCanProveMacPackagedLane": platform.system() == "Darwin",
    }
    output.write_text(json.dumps(context, indent=2) + "\n", encoding="utf-8")
    if len(sys.argv) == 3:
        start = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
        if start.get("sourceTree") != context["sourceTree"]:
            return 1
    return 0


raise SystemExit(main())

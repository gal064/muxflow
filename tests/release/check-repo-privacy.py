"""Check the public source tree for private files and likely personal details."""

from pathlib import Path
import re
import subprocess
import sys


EXCLUDED_PATHS = (
    "docs/history/qa/",
    "docs/bugs/",
    "docs/mobile/qa/",
    "docs/mobile/mocks/",
    "todos/",
    ".claude/",
)
EXCLUDED_FILES = {"todo.md", "docs/history/implementation.md"}
EXAMPLE_ACCOUNTS = {
    "ade", "agents", "alice", "deploy", "dev", "example", "linuxbrew", "me",
    "operator", "operator-extra", "someone", "test", "u", "user", "x", "zed",
}
EXAMPLE_EMAIL_DOMAINS = {
    "example.com", "example.test", "example.invalid", "github.com",
    "users.noreply.github.com",
}
EMAIL = re.compile(r"[A-Za-z0-9_.+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})")
HOME = re.compile(r"(?<![A-Za-z0-9])/(?:home|Users)/([A-Za-z0-9_.-]+)")
SECRET = re.compile(
    r"(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}"
    r"|AKIA[A-Z0-9]{16}|xox[baprs]-[A-Za-z0-9-]{10,})"
)


def main() -> int:
    paths = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"]
    ).decode().split("\0")
    findings: list[str] = []
    for name in paths:
        if not name:
            continue
        if name in EXCLUDED_FILES or name.startswith(EXCLUDED_PATHS):
            findings.append(f"{name}: internal file")
            continue
        path = Path(name)
        if not path.is_file():
            continue
        content = path.read_bytes()
        if b"\0" in content:
            continue
        text = content.decode("utf-8", errors="replace")
        for number, line in enumerate(text.splitlines(), 1):
            for match in EMAIL.finditer(line):
                domain = match.group(1).lower()
                if domain not in EXAMPLE_EMAIL_DOMAINS and not domain.endswith(".patch"):
                    findings.append(f"{name}:{number}: non-fixture email")
            for match in HOME.finditer(line):
                if match.group(1).lower() not in EXAMPLE_ACCOUNTS:
                    findings.append(f"{name}:{number}: personal home path")
            if ".test." not in name and "/tests/" not in name and SECRET.search(line):
                findings.append(f"{name}:{number}: token-shaped value")
    if findings:
        print("\n".join(findings))
        print("REPO_PRIVACY_FAILED", file=sys.stderr)
        return 1
    print("REPO_PRIVACY_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

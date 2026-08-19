import json
import sys
from pathlib import Path


def classify(arguments: str) -> str:
    for command in (
        "new-window",
        "new-session",
        "kill-window",
        "kill-session",
        "split-window",
        "attach-session",
        "display-message",
    ):
        if f"<{command}>" in arguments or arguments.startswith(f"<{command}"):
            return "batched-discovery" if command == "display-message" and "<list-sessions>" in arguments else command
    return "other"


path = Path(sys.argv[1])
counts: dict[str, int] = {}
host_lines = []
for raw in path.read_text().splitlines():
    parent, separator, arguments = raw.partition("\t")
    if not separator or parent != "tmux-ide-host":
        continue
    category = classify(arguments)
    counts[category] = counts.get(category, 0) + 1
    host_lines.append({"category": category, "arguments": arguments})

print(json.dumps({"hostTmuxProcesses": len(host_lines), "counts": counts, "ordered": host_lines}, indent=2))

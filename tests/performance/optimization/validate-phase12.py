import json
import sys
from pathlib import Path


def load(path: Path) -> dict[str, object]:
    return json.loads(path.read_text(encoding="utf-8"))


def validate_result(
    root: Path,
    filename: str,
    expected_label: str,
    expected_transport: str,
    flood_seconds: int | None,
) -> list[str]:
    path = root / filename
    if not path.is_file():
        return [f"missing {filename}"]
    value = load(path)
    errors = []
    if value.get("label") != expected_label:
        errors.append(f"{filename}: unexpected label {value.get('label')!r}")
    if value.get("transport") != expected_transport:
        errors.append(f"{filename}: unexpected transport {value.get('transport')!r}")
    if flood_seconds is not None and value.get("floodSeconds") != flood_seconds:
        errors.append(f"{filename}: unexpected flood duration {value.get('floodSeconds')!r}")
    return errors


def main() -> int:
    if len(sys.argv) != 4:
        return 2
    root = Path(sys.argv[1])
    label = sys.argv[2]
    flood_seconds = int(sys.argv[3])
    errors = []
    errors += validate_result(root, "local-app.json", f"{label}-local-app", "local", flood_seconds)
    errors += validate_result(root, "local-raw.json", f"{label}-local-raw", "local", None)
    docker_status_path = root / "docker-status.txt"
    if not docker_status_path.is_file():
        errors.append("missing docker-status.txt")
    elif docker_status_path.read_text(encoding="utf-8").strip() == "ran":
        errors += validate_result(root, "docker-app.json", f"{label}-docker-app", "ssh", flood_seconds)
        errors += validate_result(root, "docker-raw.json", f"{label}-docker-raw", "ssh", None)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    return 0


raise SystemExit(main())

#!/usr/bin/env python3
"""Summarises an in-app Phase 12 perf log into per-metric percentiles.

Usage: summarize-perf-log.py <perf.jsonl> [--json]

The log is append-only JSON lines. Sample lines carry a metric name and a
duration; rolling summary lines are ignored here because recomputing from the
raw samples is the only way to be sure the percentiles describe the whole run.
"""

import json
import sys


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    return values[round((len(values) - 1) * fraction)]


def main() -> int:
    arguments = sys.argv[1:]
    as_json = "--json" in arguments
    paths = [argument for argument in arguments if not argument.startswith("--")]
    if len(paths) != 1:
        print(__doc__.strip(), file=sys.stderr)
        return 2

    samples: dict[str, list[float]] = {}
    seen_record_ids: set[str] = set()
    malformed = 0
    with open(paths[0], encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                malformed += 1
                continue
            record_id = record.get("recordId")
            if isinstance(record_id, str):
                if record_id in seen_record_ids:
                    continue
                seen_record_ids.add(record_id)
            name = record.get("name")
            duration = record.get("ms")
            if isinstance(name, str) and isinstance(duration, (int, float)):
                samples.setdefault(name, []).append(float(duration))

    rows = []
    for name in sorted(samples):
        values = sorted(samples[name])
        rows.append(
            {
                "name": name,
                "n": len(values),
                "meanMs": round(sum(values) / len(values), 3),
                "p50Ms": round(percentile(values, 0.5), 3),
                "p95Ms": round(percentile(values, 0.95), 3),
                "maxMs": round(values[-1], 3),
            }
        )

    if as_json:
        print(json.dumps({"rows": rows, "malformedLines": malformed}, indent=2))
        return 0

    print(f"{'METRIC':<40}{'N':>7}{'MEAN':>10}{'P50':>10}{'P95':>10}{'MAX':>10}")
    for row in rows:
        print(
            f"{row['name']:<40}{row['n']:>7}{row['meanMs']:>10}"
            f"{row['p50Ms']:>10}{row['p95Ms']:>10}{row['maxMs']:>10}"
        )
    if malformed:
        print(f"\n{malformed} malformed line(s) skipped", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

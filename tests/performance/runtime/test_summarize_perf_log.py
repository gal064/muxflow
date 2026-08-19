import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class PerfLogRecoveryTest(unittest.TestCase):
    def test_partial_retry_prefix_is_malformed_and_complete_record_ids_are_deduplicated(self) -> None:
        script = Path(__file__).with_name("summarize-perf-log.py")
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "perf.jsonl"
            log.write_text(
                '{"recordId":"run:1","name":"paint","ms":10}\n'
                '{"recordId":"run:2","name":"paint"\n'
                '{"recordId":"run:1","name":"paint","ms":10}\n'
                '{"recordId":"run:2","name":"paint","ms":20}\n',
                encoding="utf-8",
            )
            result = subprocess.run(
                [sys.executable, str(script), str(log), "--json"],
                check=True,
                capture_output=True,
                text=True,
            )

        report = json.loads(result.stdout)
        self.assertEqual(report["malformedLines"], 1)
        self.assertEqual(report["rows"], [{
            "name": "paint", "n": 2, "meanMs": 15.0, "p50Ms": 10.0, "p95Ms": 20.0, "maxMs": 20.0,
        }])


if __name__ == "__main__":
    unittest.main()

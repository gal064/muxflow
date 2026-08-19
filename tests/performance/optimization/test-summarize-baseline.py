import json
import runpy
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("summarize-baseline.py")


class BaselineCoverageTests(unittest.TestCase):
    def test_failed_or_missing_lanes_never_claim_workflow_capture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "phase12").mkdir()
            (root / "logs").mkdir()
            (root / "phase12" / "docker-status.txt").write_text("ran\n", encoding="utf-8")
            previous_argv = sys.argv
            sys.argv = [str(SCRIPT), str(root), "1", "0", "0", "10"]
            try:
                with self.assertRaises(SystemExit) as exit_status:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
            finally:
                sys.argv = previous_argv
            self.assertEqual(exit_status.exception.code, 1)
            summary = json.loads((root / "summary.json").read_text(encoding="utf-8"))
            coverage = {row["journey"]: row for row in summary["measurementCoverage"]}
            self.assertEqual(coverage["workspace/tab create-open-select"]["status"], "NOT_CAPTURED")
            self.assertEqual(coverage["Explorer list/watch/expand"]["status"], "NOT_CAPTURED")
            self.assertEqual(coverage["Git status/diff/mutation"]["status"], "NOT_CAPTURED")


if __name__ == "__main__":
    unittest.main()

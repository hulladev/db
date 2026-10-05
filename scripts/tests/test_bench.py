import copy
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("bench", Path(__file__).parents[1] / "bench.py")
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


def report(samples=(990.0, 1000.0, 1010.0)):
    median = bench.statistics.median(samples)
    return {
        "format_version": 1,
        "environment": {key: "same" for key in bench.ENVIRONMENT_KEYS},
        "results": [{
            "name": "pipeline/8", "fixture_version": 1, "workload": {"fields": 8},
            "iterations_per_sample": 64, "samples_ns": list(samples),
            "median_ns": median,
            "mad_ns": bench.statistics.median(abs(value - median) for value in samples),
        }],
    }


class ComparisonTests(unittest.TestCase):
    def test_material_repeatable_slowdown_needs_review(self):
        self.assertEqual(bench.compare(report(), report((1190, 1200, 1210)))[0][3], "REVIEW")

    def test_small_absolute_change_and_noisy_change_do_not_trip_gate(self):
        self.assertEqual(bench.compare(report((90, 100, 110)), report((110, 120, 130)))[0][3], "ok")
        self.assertEqual(bench.compare(report(), report((900, 1200, 1500)))[0][3], "ok")

    def test_speedup_does_not_trip_gate(self):
        self.assertEqual(bench.compare(report(), report((490, 500, 510)))[0][3], "ok")

    def test_different_suites_database_profiles_and_sample_kinds_are_rejected(self):
        different = report()
        different["suite"] = "postgres"
        with self.assertRaisesRegex(ValueError, "suite differs"):
            bench.compare(report(), different)
        different = report()
        different["environment"]["database"] = {"server_version": "18.6"}
        with self.assertRaisesRegex(ValueError, "database environment"):
            bench.compare(report(), different)
        different = report()
        different["results"][0]["sample_kind"] = "request_latency"
        with self.assertRaisesRegex(ValueError, "fixture changed"):
            bench.compare(report(), different)

    def test_incompatible_environment_or_fixture_is_rejected(self):
        for key in bench.ENVIRONMENT_KEYS:
            changed = report()
            changed["environment"][key] = "different"
            with self.assertRaisesRegex(ValueError, "differs"):
                bench.compare(report(), changed)
        for key, value in (("fixture_version", 2), ("workload", {"fields": 32})):
            changed = report()
            changed["results"][0][key] = value
            with self.assertRaisesRegex(ValueError, "fixture changed"):
                bench.compare(report(), changed)

    def test_new_cases_are_reported_and_missing_cases_rejected(self):
        expanded = report()
        added = copy.deepcopy(expanded["results"][0])
        added["name"] = "pipeline/32"
        expanded["results"].append(added)
        self.assertEqual(bench.compare(report(), expanded)[1][3], "NEW")
        with self.assertRaisesRegex(ValueError, "disappeared"):
            bench.compare(expanded, report())

    def test_invalid_samples_and_summary_are_rejected(self):
        for value in (0.0, -1.0, float("nan"), float("inf")):
            invalid = report()
            invalid["results"][0]["samples_ns"][0] = value
            with self.assertRaisesRegex(ValueError, "invalid timing"):
                bench.validate(invalid)
        invalid = report()
        invalid["results"][0]["median_ns"] = 500
        with self.assertRaisesRegex(ValueError, "inconsistent summary"):
            bench.validate(invalid)

    def test_strict_comparison_fails_but_default_remains_advisory(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "baseline.json"
            output = Path(directory) / "current.json"
            bench.write_report(baseline, report())
            args = ["bench.py", "compare", "--baseline", str(baseline), "--output", str(output)]
            with patch.object(bench, "run_suite", return_value=report((1190, 1200, 1210))), patch("builtins.print"):
                with patch("sys.argv", args):
                    self.assertEqual(bench.main(), 0)
                with patch("sys.argv", args + ["--strict"]):
                    self.assertEqual(bench.main(), 1)
            self.assertEqual(bench.json.loads(baseline.read_text()), report())

    def test_existing_baseline_requires_explicit_replacement_reason(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "baseline.json"
            bench.write_report(baseline, report())
            args = ["bench.py", "save", "--output", str(baseline)]
            with patch.object(bench, "run_suite") as run, patch("sys.stderr"):
                with patch("sys.argv", args), self.assertRaises(SystemExit):
                    bench.main()
                run.assert_not_called()

    def test_regular_run_and_comparison_cannot_overwrite_baselines(self):
        attempts = [
            ["bench.py", "run", "--output", str(bench.BASELINE)],
            ["bench.py", "compare", "--baseline", "custom.json", "--output", "custom.json"],
        ]
        for args in attempts:
            with patch.object(bench, "run_suite") as run, patch("sys.stderr"):
                with patch("sys.argv", args), self.assertRaises(SystemExit):
                    bench.main()
                run.assert_not_called()


if __name__ == "__main__":
    unittest.main()

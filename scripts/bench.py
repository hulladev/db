#!/usr/bin/env python3
"""Run optimized benchmarks, save explicit baselines, and report practical changes."""
import argparse
import hashlib
import json
import math
import os
import platform
import statistics
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CURRENT = ROOT / "target/benchmarks/current.json"
BASELINE = ROOT / "benchmarks/baselines/local.json"
SUITES = {
    "contracts": ("hulladb_wire", "baseline", BASELINE),
    "backend": ("hulladb_postgres", "command_queries", ROOT / "benchmarks/baselines/backend-local.json"),
    "cdc": ("hulladb_cdc", "capture", ROOT / "benchmarks/baselines/cdc-snapshot-local.json"),
    "sync": ("hulladb_cdc", "sync", ROOT / "benchmarks/baselines/sync-local.json"),
}
ENVIRONMENT_KEYS = ("machine", "platform", "compiler", "build_config", "compiled_profile")


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def digest(paths):
    result = hashlib.sha256()
    for path in sorted(paths):
        result.update(str(path.relative_to(ROOT)).encode())
        result.update(b"\0")
        result.update(path.read_bytes())
    return result.hexdigest()


def environment():
    flags = {
        key: value for key, value in os.environ.items()
        if key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET")
        or key.startswith("CARGO_PROFILE_BENCH_")
    }
    configs = [path for path in (ROOT / ".cargo/config", ROOT / ".cargo/config.toml") if path.exists()]
    return {
        "machine": hashlib.sha256(platform.node().encode()).hexdigest(),
        "platform": [platform.system(), platform.release(), platform.machine(), os.cpu_count()],
        "compiler": command("rustc", "-vV"),
        "build_config": {"profile": "bench", "flags": flags, "workspace_config_sha256": digest(configs)},
    }


def run_suite(suite="contracts"):
    package, target, _ = SUITES[suite]
    compiled = command(
        "cargo", "bench", "--locked", "-p", package, "--bench", target,
        "--no-run", "--message-format=json",
    )
    executables = []
    for line in compiled.splitlines():
        artifact = json.loads(line)
        if artifact.get("reason") == "compiler-artifact" and artifact["target"]["name"] == target:
            if artifact.get("executable"):
                executables.append((artifact["executable"], artifact["profile"]))
    if len(executables) != 1:
        raise ValueError("expected one baseline benchmark executable")
    executable, profile = executables[0]
    payload = json.loads(command(executable, "--bench"))
    database = payload.pop("database", None)
    payload["suite"] = suite
    payload["environment"] = environment()
    payload["environment"]["compiled_profile"] = profile
    if database is not None:
        payload["environment"]["database"] = database
    payload["provenance"] = {
        "measured_at_utc": datetime.now(timezone.utc).isoformat(),
        "git_head": command("git", "rev-parse", "HEAD"),
        "source_sha256": digest(
            list((ROOT / "crates").rglob("*.rs"))
            + list((ROOT / "crates").rglob("*.sql"))
            + list((ROOT / "crates").rglob("Cargo.toml"))
            + [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "rust-toolchain.toml"]
        ),
        "harness_sha256": digest([ROOT / f"crates/{package}/benches/{target}.rs"]),
        "runner_sha256": digest([Path(__file__).resolve()]),
    }
    validate(payload)
    return payload


def validate(payload):
    if payload.get("format_version") != 1 or not payload.get("results"):
        raise ValueError("unsupported or empty benchmark report")
    if any(key not in payload.get("environment", {}) for key in ENVIRONMENT_KEYS):
        raise ValueError("report is missing environment metadata")
    names = set()
    for result in payload["results"]:
        if result["name"] in names:
            raise ValueError("duplicate benchmark name: " + result["name"])
        names.add(result["name"])
        samples = result["samples_ns"]
        if len(samples) < 3 or any(not math.isfinite(value) or value <= 0 for value in samples):
            raise ValueError("invalid timing samples: " + result["name"])
        median = statistics.median(samples)
        mad = statistics.median(abs(value - median) for value in samples)
        if not math.isclose(result["median_ns"], median) or not math.isclose(result["mad_ns"], mad):
            raise ValueError("inconsistent summary: " + result["name"])
        if result["iterations_per_sample"] < 1 or result["fixture_version"] < 1:
            raise ValueError("invalid fixture or iteration count")


def compare(baseline, current, threshold=15.0, minimum_ns=100.0):
    validate(baseline)
    validate(current)
    if baseline.get("suite", "contracts") != current.get("suite", "contracts"):
        raise ValueError("benchmark suite differs")
    if baseline["environment"].get("database") != current["environment"].get("database"):
        raise ValueError("database environment differs; establish a separate baseline")
    if any(baseline["environment"][key] != current["environment"][key] for key in ENVIRONMENT_KEYS):
        raise ValueError("machine, OS, compiler, or build configuration differs; establish a baseline for this environment")
    old = {result["name"]: result for result in baseline["results"]}
    new = {result["name"]: result for result in current["results"]}
    missing = old.keys() - new.keys()
    if missing:
        raise ValueError("baseline cases disappeared: " + ", ".join(sorted(missing)))
    changes = []
    for name, result in new.items():
        if name not in old:
            changes.append((name, None, None, "NEW"))
            continue
        previous = old[name]
        if any(previous[key] != result[key] for key in ("fixture_version", "workload")) or previous.get("sample_kind", "batch_average") != result.get("sample_kind", "batch_average"):
            raise ValueError("fixture changed: " + name + "; review and explicitly establish a new baseline")
        delta = result["median_ns"] - previous["median_ns"]
        percent = 100 * delta / previous["median_ns"]
        # Practical review heuristic, not a statistical significance test.
        noise = 3 * (previous["mad_ns"] + result["mad_ns"])
        review = percent >= threshold and delta >= minimum_ns and delta > noise
        changes.append((name, percent, delta, "REVIEW" if review else "ok"))
    return changes


def write_report(path, payload):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n")


def positive_number(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be a finite positive number")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="mode", required=True)
    run = commands.add_parser("run", help="measure without changing the baseline")
    run.add_argument("--output", type=Path)
    save = commands.add_parser("save", help="explicitly create a baseline")
    save.add_argument("--output", type=Path)
    save.add_argument("--replace", action="store_true")
    save.add_argument("--reason", help="required when replacing an existing baseline")
    comparison = commands.add_parser("compare", help="measure and compare with a saved baseline")
    comparison.add_argument("--baseline", type=Path)
    comparison.add_argument("--output", type=Path)
    comparison.add_argument("--threshold", type=positive_number, default=15.0)
    comparison.add_argument("--minimum-ns", type=positive_number, default=100.0)
    comparison.add_argument("--strict", action="store_true", help="exit 1 when a case needs review")
    for subparser in (run, save, comparison):
        subparser.add_argument("--suite", choices=SUITES, default="contracts")
    args = parser.parse_args()
    default_baseline = SUITES[args.suite][2]
    if args.output is None:
        args.output = default_baseline if args.mode == "save" else (CURRENT if args.suite == "contracts" else ROOT / f"target/benchmarks/{args.suite}-current.json")
    if args.mode == "compare" and args.baseline is None:
        args.baseline = default_baseline
    if args.mode != "save" and (ROOT / "benchmarks/baselines").resolve() in (args.output.resolve(), *args.output.resolve().parents):
        parser.error("use save to write inside the baseline directory")
    if args.mode == "compare" and args.output.resolve() == args.baseline.resolve():
        parser.error("comparison output cannot overwrite its baseline")
    if args.mode == "save" and args.output.exists() and not (args.replace and args.reason and args.reason.strip()):
        parser.error("baseline already exists; replacement requires --replace --reason")
    try:
        baseline = None
        if args.mode == "compare":
            baseline = json.loads(args.baseline.read_text())
            validate(baseline)
        current = run_suite(args.suite)
        if args.mode == "save":
            current["baseline_reason"] = args.reason or "initial baseline"
        changes = compare(baseline, current, args.threshold, args.minimum_ns) if baseline else []
        write_report(args.output, current)
        for result in current["results"]:
            tail = f" p95 {result['p95_ns'] / 1000:.3f} us" if "p95_ns" in result else ""
            print(f"{result['name']:32} {result['median_ns'] / 1000:9.3f} us/op{tail}")
        if baseline:
            print("\nChanges against baseline (median time per operation):")
            for name, percent, delta, status in changes:
                timing = "new case" if percent is None else f"{percent:+7.1f}% {delta / 1000:+9.3f} us"
                print(f"{name:32} {timing:26} {status}")
            reviews = sum(status == "REVIEW" for _, _, _, status in changes)
            print(f"\n{reviews} case(s) need review; repeat flagged runs before drawing conclusions.")
            if args.strict and reviews:
                return 1
        print(f"\nSaved {args.output}")
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"benchmark error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

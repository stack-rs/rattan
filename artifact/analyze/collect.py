#!/usr/bin/env python3

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import read_fct, read_throughput

SCENARIO_PATTERNS = {
    "throughput": (
        re.compile(r"^tput-(?P<mbps>\d+)mbps$"),
        lambda m: {"x": int(m["mbps"]) * 1000},
    ),
    "complexity": (
        re.compile(r"^complexity-(?P<depth>\d+)$"),
        lambda m: {"x": int(m["depth"])},
    ),
    "density": (
        re.compile(r"^density-(?P<mbps>\d+)mbps-(?P<mode>static|trace)-c(?P<n>\d+)$"),
        lambda m: {"x": int(m["n"]), "mode": m["mode"]},
    ),
}

X_LABEL = {
    "throughput": "configured bandwidth, Kbit/s",
    "complexity": "path complexity, stages",
    "density": "concurrent instances",
}


def report_gaps(samples: list[dict], skipped: list[dict]) -> None:
    counted: dict[str, list[int]] = {}
    for sample in samples:
        key = f"{sample['emulator']}/{sample['scenario']}"
        counted.setdefault(key, [0, 0])[0] += 1
    for entry in skipped:
        key = str(Path(entry["log"]).parent)
        counted.setdefault(key, [0, 0])[1] += 1

    for key in sorted(counted):
        kept, lost = counted[key]
        if lost:
            print(f"warning: {key}: {kept} of {kept + lost} trials measured anything")


def collect(experiment: str, run_dir: Path) -> dict:
    pattern, extract = SCENARIO_PATTERNS[experiment]

    samples: list[dict] = []
    skipped: list[dict] = []
    unknown: set[str] = set()

    for log in sorted(run_dir.glob("*/*/*.log")):
        emulator = log.parent.parent.name
        scenario = log.parent.name
        match = pattern.fullmatch(scenario)
        if match is None:
            unknown.add(scenario)
            continue

        value, reason = read_throughput(log, fill_short_runs=experiment == "density")
        rel = str(log.relative_to(run_dir))
        if value is None:
            skipped.append({"log": rel, "reason": reason})
            continue

        samples.append(
            {"emulator": emulator, "scenario": scenario, "value": value, "log": rel}
            | extract(match)
        )

    for scenario in sorted(unknown):
        print(f"warning: ignoring '{scenario}', not a {experiment} scenario name")

    report_gaps(samples, skipped)

    return {
        "experiment": experiment,
        "run": run_dir.name,
        "unit": "Kbit/s",
        "x_label": X_LABEL[experiment],
        "estimator": (
            "the mean of the one-second iperf3 samples after the first three, "
            "which are discarded as warm-up; uncorrected for header overhead"
        )
        + (
            "; a trial cut short by the group's deadline is averaged over the "
            "seconds it was asked for, so the seconds it never got count as "
            "the zero throughput they carried"
            if experiment == "density"
            else ""
        ),
        "samples": samples,
        "skipped": skipped,
    }


def collect_mptcp(run_dir: Path) -> dict:
    samples: list[dict] = []
    skipped: list[dict] = []

    for log in sorted(run_dir.glob("*/*/*/sender.log")):
        combination = log.parent.parent.name
        scenario = log.parent.parent.parent.name
        if "_" not in combination:
            print(f"warning: ignoring '{combination}', not a <scheduler>_<cca> name")
            continue
        scheduler, cca = combination.rsplit("_", 1)

        value, reason = read_fct(log)
        rel = str(log.relative_to(run_dir))
        if value is None:
            skipped.append({"log": rel, "reason": reason})
            continue

        samples.append(
            {
                "scenario": scenario.upper(),
                "scheduler": scheduler,
                "cca": cca,
                "value": value,
                "log": rel,
            }
        )

    return {
        "experiment": "mptcp",
        "run": run_dir.name,
        "unit": "s",
        "x_label": "scenario, scheduler and congestion control algorithm",
        "estimator": (
            "the time the sender reports for transferring the whole file, "
            "measured from before the connection is opened"
        ),
        "samples": samples,
        "skipped": skipped,
    }


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: collect.py <experiment> <run-directory>", file=sys.stderr)
        return 1
    experiment, run_dir = sys.argv[1], Path(sys.argv[2])

    if experiment not in SCENARIO_PATTERNS and experiment != "mptcp":
        print(f"unknown experiment '{experiment}'", file=sys.stderr)
        return 1
    if not run_dir.is_dir():
        print(f"no such directory: {run_dir}", file=sys.stderr)
        return 1

    if experiment == "mptcp":
        result = collect_mptcp(run_dir)
    else:
        result = collect(experiment, run_dir)
    out = run_dir / "result.json"
    out.write_text(json.dumps(result, indent=2) + "\n")

    kept, lost = len(result["samples"]), len(result["skipped"])
    print(
        f"{out}: {kept} samples"
        + (f", {lost} logs without a measurement" if lost else "")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

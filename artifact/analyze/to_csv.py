#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path, PurePosixPath

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import correction_for, fmt, summarize, write_csv

SPECS: dict[str, list[dict]] = {
    "throughput": [
        {
            "file": "figure5-throughput.csv",
            "x_header": "Bandwidth Setting",
            "columns": [
                ("rattan", "rattan-rv"),
                ("mahimahi", "mahimahi"),
                ("mininet", "mininet"),
            ],
            "drop_outliers": True,
        }
    ],
    "complexity": [
        {
            "file": "figure6-complexity.csv",
            "x_header": "Complexity",
            "columns": [
                ("mininet", "mininet"),
                ("mahimahi", "mahimahi"),
                ("rattan", "rattan-rv"),
            ],
            "drop_outliers": True,
        }
    ],
    "density": [
        {
            "file": "figure7a-density-static.csv",
            "x_header": "Concurrency",
            "mode": "static",
            "columns": [
                ("mahimahi", "mahimahi"),
                ("mininet", "mininet"),
                ("rattan", "rattan"),
            ],
            "drop_outliers": False,
            "failures_are_zero": True,
        },
        {
            "file": "figure7b-density-trace.csv",
            "x_header": "Concurrency",
            "mode": "trace",
            "columns": [
                ("mahimahi", "mahimahi"),
                ("rattan", "rattan"),
            ],
            "drop_outliers": False,
            "failures_are_zero": True,
        },
    ],
}


MPTCP_SCENARIOS = ["A", "B", "C", "D"]
MPTCP_SCHEDULERS = ["minRTT", "blest", "ecf"]
MPTCP_CCAS = ["cubic", "bbr", "vegas", "lia", "olia", "balia"]


def ordered(values: set[str], preferred: list[str]) -> list[str]:
    known = [v for v in preferred if v in values]
    return known + sorted(values - set(known))


def build_mptcp(result: dict) -> tuple[list[str], list[list[str]]]:
    grouped: dict[tuple[str, str, str], list[float]] = defaultdict(list)
    for sample in result["samples"]:
        key = (sample["scenario"], sample["scheduler"], sample["cca"])
        grouped[key].append(sample["value"])

    scenarios = ordered({k[0] for k in grouped}, MPTCP_SCENARIOS)
    schedulers = ordered({k[1] for k in grouped}, MPTCP_SCHEDULERS)
    ccas = ordered({k[2] for k in grouped}, MPTCP_CCAS)

    header = ["scenario", "scheduler", "cca", "fct_mean_s", "fct_std_s"]
    rows: list[list[str]] = []
    for scenario in scenarios:
        for scheduler in schedulers:
            for cca in ccas:
                runs = grouped.get((scenario, scheduler, cca))
                if not runs:
                    continue
                mean, std, _ = summarize(runs, drop_outliers=False)
                rows.append([scenario, scheduler, cca, fmt(mean), fmt(std)])
    return header, rows


def corrected_samples(
    result: dict, mode: str | None, count_failures_as_zero: bool = False
) -> dict[int, dict[str, list[float]]]:
    grouped: dict[int, dict[str, list[float]]] = defaultdict(lambda: defaultdict(list))
    for sample in result["samples"]:
        if mode is not None and sample.get("mode") != mode:
            continue
        factor = correction_for(sample["emulator"])
        grouped[sample["x"]][sample["emulator"]].append(sample["value"] * factor)

    if not count_failures_as_zero:
        return grouped

    for entry in result.get("skipped", []):
        emulator, scenario = PurePosixPath(entry["log"]).parts[:2]
        parsed = scenario_x(scenario)
        if parsed is None or (mode is not None and parsed[1] != mode):
            continue
        grouped[parsed[0]][emulator].append(0.0)
    return grouped


DENSITY_SCENARIO_RE = re.compile(
    r"^density-\d+mbps-(?P<mode>static|trace)-c(?P<n>\d+)$"
)


def scenario_x(scenario: str) -> tuple[int, str] | None:
    match = DENSITY_SCENARIO_RE.fullmatch(scenario)
    return (int(match["n"]), match["mode"]) if match else None


def build(result: dict, spec: dict) -> tuple[list[str], list[list[str]]]:
    grouped = corrected_samples(
        result, spec.get("mode"), spec.get("failures_are_zero", False)
    )

    header = [spec["x_header"]]
    for _, name in spec["columns"]:
        header += [f"{name}_mean", f"{name}_std"]

    rows: list[list[str]] = []
    for x in sorted(grouped):
        cells = [str(x)]
        any_value = False
        for key, _ in spec["columns"]:
            mean, std, _ = summarize(grouped[x].get(key, []), spec["drop_outliers"])
            any_value = any_value or mean is not None
            cells += [fmt(mean), fmt(std)]
        if any_value:
            rows.append(cells)
    return header, rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("run_dir", type=Path, help="run directory holding result.json")
    args = parser.parse_args()

    result_path = args.run_dir / "result.json"
    if not result_path.is_file():
        print(
            f"no result.json in {args.run_dir}; run collect.py first", file=sys.stderr
        )
        return 1
    result = json.loads(result_path.read_text())

    if result["experiment"] == "mptcp":
        header, rows = build_mptcp(result)
        if not rows:
            print("no runs finished, not writing table2-mptcp-fct.csv")
            return 1
        out = args.run_dir / "table2-mptcp-fct.csv"
        write_csv(out, header, rows)
        print(f"{out}: {len(rows)} rows")
        return 0

    specs = SPECS.get(result["experiment"])
    if specs is None:
        print(f"unknown experiment '{result['experiment']}'", file=sys.stderr)
        return 1

    for spec in specs:
        header, rows = build(result, spec)
        if not rows:
            print(f"no samples for {spec['file']}, not writing it")
            continue
        out = args.run_dir / spec["file"]
        write_csv(out, header, rows)
        print(f"{out}: {len(rows)} rows")
    return 0


if __name__ == "__main__":
    sys.exit(main())

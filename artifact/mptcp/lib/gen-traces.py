#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
from pathlib import Path

CONFIG_DIR = Path(__file__).resolve().parent.parent / "config"

TRACES = ["high-variance", "low-variance"]


def load(name: str) -> list[tuple[float, int]]:
    raw = json.loads((CONFIG_DIR / f"trace-{name}.json").read_text())
    return [(float(multiple), int(ms)) for multiple, ms in raw]


def check_mean(steps: list[tuple[float, int]], name: str) -> None:
    total_ms = sum(ms for _, ms in steps)
    mean = sum(multiple * ms for multiple, ms in steps) / total_ms
    if abs(mean - 1.0) > 1e-6:
        raise ValueError(f"trace-{name}.json averages {mean:.6f}, not 1.0")


def bw_replay_config(steps: list[tuple[float, int]], mbps: float) -> dict:
    return {
        "RepeatedBwPatternConfig": {
            "pattern": [
                {
                    "TraceBwConfig": {
                        "pattern": [
                            [f"{ms}ms", [f"{multiple * mbps:.3f}Mbps"]]
                            for multiple, ms in steps
                        ]
                    }
                }
            ],
            "count": 0,
        }
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--mbps",
        type=float,
        default=60,
        help="mean bandwidth to scale each recording to (default 60)",
    )
    args = parser.parse_args()

    for name in TRACES:
        steps = load(name)
        check_mean(steps, name)
        out = CONFIG_DIR / f"bw-{int(args.mbps)}mbps-trace-{name}.json"
        out.write_text(json.dumps(bw_replay_config(steps, args.mbps), indent=4) + "\n")
        seconds = sum(ms for _, ms in steps) / 1000
        print(
            f"{out.name}: {len(steps)} steps, {seconds:.1f} s, {args.mbps:g} Mbps mean"
        )


if __name__ == "__main__":
    main()

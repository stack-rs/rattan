#!/usr/bin/env python3

from __future__ import annotations

import csv
import math
import re
from pathlib import Path
from statistics import fmean, median, pstdev

INTERVAL_RE = re.compile(
    r"^\[\s*\d+\]\s+(\d+\.\d+)-(\d+\.\d+)\s+sec\s+.*?([\d.]+)\s+Kbits/sec"
)
SEPARATOR = "- - -"

MIN_INTERVAL_SECONDS = 0.5

# Discard the first three one-second samples as slow-start warm-up (§5.1).
WARMUP_SAMPLES = 3
MIN_SAMPLES = 2

ASKED_RE = re.compile(r"^#.*\bseconds=(\d+)\b")

NOT_STARTED = "missed the group's window"


def read_throughput(
    path: Path, fill_short_runs: bool = False
) -> tuple[float | None, str]:
    text = path.read_text(errors="replace")
    samples: list[float] = []
    asked: int | None = None
    for line in text.splitlines():
        if line.startswith(SEPARATOR):
            break
        if line.startswith("#"):
            match = ASKED_RE.match(line)
            if match:
                asked = int(match.group(1))
            continue
        match = INTERVAL_RE.match(line)
        if match is None:
            continue
        start, end, rate = (float(group) for group in match.groups())
        if end - start >= MIN_INTERVAL_SECONDS:
            samples.append(rate)

    usable = samples[WARMUP_SAMPLES:]
    if not fill_short_runs:
        if len(usable) < MIN_SAMPLES:
            return None, (
                f"{len(samples)} one-second samples, "
                f"{MIN_SAMPLES + WARMUP_SAMPLES} needed"
            )
        return fmean(usable), ""

    if not usable:
        if NOT_STARTED in text:
            return None, "started too late for the group's window"
        return None, f"{len(samples)} one-second samples, none past the warm-up"
    expected = max(asked - WARMUP_SAMPLES, len(usable)) if asked else len(usable)
    return sum(usable) / expected, ""


FCT_RE = re.compile(
    r"=+RCT_RESULT=+\s+Sent\s+(\d+)\s+bytes to server in\s+([\d.]+)\s*s\."
)


def read_fct(path: Path) -> tuple[float | None, str]:
    for line in path.read_text(errors="replace").splitlines():
        match = FCT_RE.search(line)
        if match:
            return float(match.group(2)), ""
    return None, "the transfer did not finish"


# Header overhead: iperf3 counts TCP payload, the emulators shape whole packets.
MSS = 1448

CORRECTION = {
    "rattan": 1500 / MSS,
    "rattan-rv": 1500 / MSS,
    "mahimahi": 1500 / MSS,
    "mininet": 1514 / MSS,
}


def correction_for(emulator: str) -> float:
    try:
        return CORRECTION[emulator]
    except KeyError:
        raise KeyError(
            f"no header-overhead factor for '{emulator}'; add one to CORRECTION"
        ) from None


# Outliers: modified z-score above 3.5 (Iglewicz and Hoaglin, 1993).
ROBUST_Z_THRESHOLD = 3.5


def filter_outliers(
    raw: list[float], threshold: float = ROBUST_Z_THRESHOLD
) -> list[float]:
    if len(raw) < 3:
        return list(raw)
    med = median(raw)
    deviations = [abs(x - med) for x in raw]
    mad = median(deviations)
    if mad > 0:
        scores = [0.6745 * (x - med) / mad for x in raw]
    else:
        mean_ad = fmean(deviations)
        if mean_ad == 0:
            return list(raw)
        scores = [(x - med) / (1.253314 * mean_ad) for x in raw]
    return [x for x, s in zip(raw, scores) if abs(s) <= threshold]


def summarize(
    raw: list[float], drop_outliers: bool
) -> tuple[float | None, float | None, int]:
    kept = filter_outliers(raw) if drop_outliers else list(raw)
    if not kept:
        return None, None, 0
    if len(kept) == 1:
        return float(kept[0]), 0.0, 1
    return fmean(kept), pstdev(kept), len(kept)


def fmt(value: float | None) -> str:
    if value is None or not math.isfinite(value):
        return ""
    return f"{value:.10g}"


def write_csv(path: Path, header: list[str], rows: list[list[str]]) -> None:
    with path.open("w", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(header)
        writer.writerows(rows)


def read_csv(path: Path) -> tuple[list[str], list[dict[str, str]]]:
    with path.open(newline="") as handle:
        reader = csv.DictReader(handle)
        header = list(reader.fieldnames or [])
        return header, list(reader)


COLUMN_ALIASES = {
    "rattan": ["rattan", "rattan-rv"],
    "rattan-rv": ["rattan-rv", "rattan-rv_less-yield", "rattan"],
}


def emulators_in(header: list[str]) -> list[str]:
    return [name[: -len("_mean")] for name in header if name.endswith("_mean")]


def resolve_column(emulator: str, header: list[str]) -> str | None:
    for candidate in COLUMN_ALIASES.get(emulator, [emulator]):
        if f"{candidate}_mean" in header:
            return candidate
    return None


def series(
    rows: list[dict[str, str]], x_key: str, column: str
) -> list[tuple[float, float, float | None]]:
    out: list[tuple[float, float, float | None]] = []
    for row in rows:
        mean = row.get(f"{column}_mean", "")
        if mean in ("", None):
            continue
        std = row.get(f"{column}_std", "")
        out.append((float(row[x_key]), float(mean), float(std) if std else None))
    return sorted(out)

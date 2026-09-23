#!/usr/bin/env python3

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import matplotlib  # ty: ignore[unresolved-import]

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # ty: ignore[unresolved-import]
from common import emulators_in, read_csv, resolve_column, series

FIGURES = [
    {
        "csv": "figure5-throughput.csv",
        "png": "figure5-throughput.png",
        "title": "Figure 5: single-channel forwarding throughput, 50 ms RTT",
        "x_label": "Bandwidth Setting (Gbps)",
        "y_label": "Achieved Tput (Gbps)",
        "x_scale": 1e6,
        "scale": 1e6,
        "target": "diagonal",
        "log_x": False,
    },
    {
        "csv": "figure6-complexity.csv",
        "png": "figure6-complexity.png",
        "title": "Figure 6: throughput versus path complexity, 1 Gbps target",
        "x_label": "Complexity N",
        "y_label": "Achieved Tput (Gbps)",
        "x_scale": 1,
        "scale": 1e6,
        "target": 1.0,
        "log_x": False,
    },
    {
        "csv": "figure7a-density-static.csv",
        "png": "figure7a-density-static.png",
        "title": "Figure 7(a): density with static bandwidth, 16 Mbps target",
        "x_label": "Concurrent Instances",
        "y_label": "Achieved Tput (Mbps)",
        "x_scale": 1,
        "scale": 1e3,
        "target": 16.0,
        "log_x": True,
    },
    {
        "csv": "figure7b-density-trace.csv",
        "png": "figure7b-density-trace.png",
        "title": "Figure 7(b): density with trace bandwidth, 16 Mbps mean target",
        "x_label": "Concurrent Instances",
        "y_label": "Achieved Tput (Mbps)",
        "x_scale": 1,
        "scale": 1e3,
        "target": 16.0,
        "log_x": True,
    },
]

COLORS = {
    "rattan": "tab:blue",
    "rattan-rv": "tab:blue",
    "rattan-rv_less-yield": "tab:blue",
    "mahimahi": "tab:orange",
    "mininet": "tab:green",
}


def plot_one(
    spec: dict, csv_dir: Path, out_dir: Path, reference: Path | None
) -> Path | None:
    path = csv_dir / spec["csv"]
    if not path.is_file():
        return None
    header, rows = read_csv(path)
    x_key = header[0]

    figure, axes = plt.subplots(figsize=(7, 4.2))

    for emulator in emulators_in(header):
        column = resolve_column(emulator, header) or emulator
        points = series(rows, x_key, column)
        if not points:
            continue
        xs = [x / spec["x_scale"] for x, _, _ in points]
        means = [mean / spec["scale"] for _, mean, _ in points]
        stds = [(std or 0.0) / spec["scale"] for _, _, std in points]
        color = COLORS.get(emulator, None)
        axes.plot(xs, means, marker="o", markersize=3, color=color, label=emulator)
        axes.fill_between(
            xs,
            [m - s for m, s in zip(means, stds)],
            [m + s for m, s in zip(means, stds)],
            color=color,
            alpha=0.2,
            linewidth=0,
        )

        blanks = [
            float(row[x_key]) / spec["x_scale"]
            for row in rows
            if not (row.get(f"{column}_mean") or "").strip()
        ]
        if blanks:
            axes.plot(
                blanks,
                [0.0] * len(blanks),
                marker="x",
                markersize=7,
                linestyle="none",
                color=color,
                label=f"{emulator} (nothing completed)",
            )

    if (
        reference is not None
        and (reference / spec["csv"]).is_file()
        and csv_dir.resolve() != reference.resolve()
    ):
        ref_header, ref_rows = read_csv(reference / spec["csv"])
        for emulator in emulators_in(ref_header):
            points = series(ref_rows, ref_header[0], emulator)
            if not points:
                continue
            axes.plot(
                [x / spec["x_scale"] for x, _, _ in points],
                [mean / spec["scale"] for _, mean, _ in points],
                color=COLORS.get(emulator, "gray"),
                alpha=0.4,
                linewidth=1,
                linestyle=":",
                zorder=0,
                label=f"{emulator} (reference)",
            )

    if spec["target"] == "diagonal":
        limits = axes.get_xlim()
        axes.plot(limits, limits, "k--", linewidth=1, label="configured")
        axes.set_xlim(limits)
    else:
        axes.axhline(
            spec["target"], color="k", linestyle="--", linewidth=1, label="configured"
        )

    if spec["log_x"]:
        axes.set_xscale("log", base=2)
        xs_all = sorted({float(row[x_key]) for row in rows})
        ticks = [x for x in xs_all if x > 0 and (int(x) & (int(x) - 1)) == 0]
        axes.set_xticks(ticks or xs_all)
        axes.set_xticklabels([f"{int(x)}" for x in (ticks or xs_all)])
        axes.minorticks_off()

    axes.set_title(spec["title"], fontsize=10)
    axes.set_xlabel(spec["x_label"])
    axes.set_ylabel(spec["y_label"])
    axes.set_ylim(bottom=0)
    axes.grid(alpha=0.3)
    axes.legend(fontsize=8)
    figure.tight_layout()

    out = out_dir / spec["png"]
    figure.savefig(out, dpi=150)
    plt.close(figure)
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("csv_dir", type=Path)
    parser.add_argument(
        "--reference",
        type=Path,
        default=None,
        help="a directory of CSVs to draw underneath as dotted lines",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help="where to write the PNGs (default: csv_dir)",
    )
    args = parser.parse_args()

    out_dir = args.out or args.csv_dir
    out_dir.mkdir(parents=True, exist_ok=True)

    written = [
        plot_one(spec, args.csv_dir, out_dir, args.reference) for spec in FIGURES
    ]
    written = [path for path in written if path is not None]
    if not written:
        print(f"no figure*.csv in {args.csv_dir}", file=sys.stderr)
        return 1
    for path in written:
        print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Render cognitive-fidelity charts from target/fidelity/*.csv (matplotlib).

Run after `cargo bench --bench fidelity_report`:
    pip install matplotlib
    python3 scripts/plot_fidelity.py
Outputs one PNG per paradigm into target/fidelity/.
"""
import csv
import collections
import pathlib

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

OUT = pathlib.Path("target/fidelity")


def load(name):
    series = collections.defaultdict(lambda: ([], []))
    with open(OUT / f"{name}.csv") as f:
        for row in csv.DictReader(f):
            xs, ys = series[row["series"]]
            xs.append(float(row["x"]))
            ys.append(float(row["y"]))
    return series


def plot(name, logx=False):
    series = load(name)
    plt.figure(figsize=(6, 4))
    for label, (xs, ys) in series.items():
        plt.plot(xs, ys, marker="o", label=label)
    if logx:
        plt.xscale("log")
    plt.title(name)
    plt.legend()
    plt.grid(True, alpha=0.3)
    plt.tight_layout()
    plt.savefig(OUT / f"{name}.png", dpi=130)
    plt.close()
    print(f"wrote {OUT / f'{name}.png'}")


if __name__ == "__main__":
    plot("forgetting", logx=True)
    for n in ("fan_effect", "priming", "interference", "commitment", "spacing"):
        plot(n)

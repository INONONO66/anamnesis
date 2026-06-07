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


def plot_spacing():
    # The spacing effect lives in a small margin (~0.05) on top of a large
    # retained_action baseline (~11), so a single shared axis hides it. Split into
    # (1) a zoomed recency-controlled comparison and (2) the margin-vs-retention-
    # interval crossover, which a pure recency model cannot produce.
    s = load("spacing")
    _, cl = s["recency_controlled_day40"]  # [clustered, spaced]
    clustered, spaced = cl[0], cl[1]
    ri_x, ri = s["spaced_minus_clustered_by_RI"]  # margins at day 30, 40

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(11, 4))

    # Left: zoomed bars so the (small) margin is visible.
    bars = ax1.bar(
        ["clustered\n[23,24,25]", "spaced\n[1,13,25]"],
        [clustered, spaced],
        color=["#d62728", "#1f77b4"],
    )
    lo, hi = min(cl), max(cl)
    pad = max((hi - lo) * 0.6, 1e-3)
    ax1.set_ylim(lo - pad, hi + pad)
    ax1.set_ylabel("retained_action @ day 40")
    ax1.set_title(
        "recency-controlled (same last study day 25)\n"
        f"Δ = spaced − clustered = {spaced - clustered:+.4f}"
    )
    for b, v in zip(bars, [clustered, spaced]):
        ax1.text(b.get_x() + b.get_width() / 2, v, f"{v:.4f}", ha="center", va="bottom")

    # Right: the crossover — spaced overtakes clustered only at a delayed test.
    ax2.axhline(0, color="gray", lw=1, ls="--")
    ax2.plot(ri_x, ri, marker="o", color="#1f77b4")
    ax2.set_xlabel("test day (retention interval)")
    ax2.set_ylabel("spaced − clustered")
    ax2.set_title("spacing × retention-interval crossover")
    ax2.set_xticks(ri_x)
    for x, y in zip(ri_x, ri):
        ax2.annotate(
            f"{y:+.4f}", (x, y), textcoords="offset points",
            xytext=(0, 10 if y >= 0 else -14), ha="center",
        )
    ymax = max(abs(v) for v in ri) * 1.6
    ax2.set_ylim(-ymax, ymax)

    fig.suptitle("spacing")
    fig.tight_layout()
    fig.savefig(OUT / "spacing.png", dpi=130)
    plt.close(fig)
    print(f"wrote {OUT / 'spacing.png'}")


def plot_priming():
    # Two distinct claims: (1) related cue > unrelated (semantic priming), and
    # (2) two converging paths sum above one (additive activation). Separate panels.
    s = load("priming")
    _, pr = s["priming"]         # [unrelated, related]
    _, ps = s["path_summation"]  # [one path, two paths]

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(10, 4))
    b1 = ax1.bar(["unrelated", "related"], pr, color=["#7f7f7f", "#1f77b4"])
    ax1.set_title("semantic priming (related > unrelated)")
    ax1.set_ylabel("target activation")
    for b, v in zip(b1, pr):
        ax1.text(b.get_x() + b.get_width() / 2, v, f"{v:.4f}", ha="center", va="bottom")

    b2 = ax2.bar(["one path", "two paths"], ps, color=["#7f7f7f", "#1f77b4"])
    ax2.set_title("additive path summation (two > one)")
    ax2.set_ylabel("target activation")
    for b, v in zip(b2, ps):
        ax2.text(b.get_x() + b.get_width() / 2, v, f"{v:.4f}", ha="center", va="bottom")

    fig.suptitle("priming")
    fig.tight_layout()
    fig.savefig(OUT / "priming.png", dpi=130)
    plt.close(fig)
    print(f"wrote {OUT / 'priming.png'}")


if __name__ == "__main__":
    plot("forgetting", logx=True)
    for n in ("fan_effect", "interference", "commitment"):
        plot(n)
    plot_priming()
    plot_spacing()

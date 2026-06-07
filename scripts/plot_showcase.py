#!/usr/bin/env python3
"""Render the cognitive-fidelity showcase charts from the engine-produced CSVs.

Reads ``target/fidelity-showcase/*.csv`` (written by ``cargo bench --bench
fidelity_showcase``) and writes committed PNGs into
``docs/07-quality-gates/assets/``. Pure matplotlib (Agg backend), deterministic,
no network. Every plotted point is a real engine number; the only computed
overlays are least-squares fits and the recency-only reference line, all labelled
as such.

Run:
    cargo bench --bench fidelity_showcase
    python3 scripts/plot_showcase.py
"""

import csv
import math
import os

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CSV_DIR = os.path.join(REPO, "target", "fidelity-showcase")
ASSET_DIR = os.path.join(REPO, "docs", "07-quality-gates", "assets")


def read_csv(name):
    """Return (header, rows-of-floats) for a showcase CSV."""
    path = os.path.join(CSV_DIR, name)
    with open(path, newline="") as fh:
        reader = csv.reader(fh)
        header = next(reader)
        rows = [[float(c) for c in row] for row in reader if row]
    return header, rows


def column(header, rows, name):
    idx = header.index(name)
    return [r[idx] for r in rows]


def linreg(xs, ys):
    """Ordinary least squares slope, intercept for y = slope*x + intercept."""
    n = len(xs)
    sx = sum(xs)
    sy = sum(ys)
    sxx = sum(x * x for x in xs)
    sxy = sum(x * y for x, y in zip(xs, ys))
    denom = n * sxx - sx * sx
    slope = 0.0 if abs(denom) < 1e-18 else (n * sxy - sx * sy) / denom
    intercept = (sy - slope * sx) / n
    return slope, intercept


def r2(obs, pred):
    mean = sum(obs) / len(obs)
    ss_tot = sum((o - mean) ** 2 for o in obs)
    ss_res = sum((o - p) ** 2 for o, p in zip(obs, pred))
    return 1.0 - ss_res / ss_tot if ss_tot > 0 else 0.0


def plot_forgetting():
    """retained_action vs delay (log x), with power-law and exponential LS fits.

    The engine computes the base-level reservoir B = -d*ln(Δt), so the curve is
    log-linear by construction; the power-law (linear in ln t) fit should hug the
    points while the exponential (linear in t) fit cannot. We report both r².
    """
    header, rows = read_csv("forgetting.csv")
    delay = column(header, rows, "delay_days")
    ra = column(header, rows, "retained_action")

    # Power-law base-level form: the engine's B = -d*ln(Δt), i.e. retained_action is
    # LINEAR in ln t. Fit B = m*ln t + c; the slope m is the calibrated forgetting
    # rate -m_type*alpha (= -0.40 for Episodic), the signature of power-law decay.
    lx = [math.log(t) for t in delay]
    slope_power, icpt_power = linreg(lx, ra)
    pred_power = [slope_power * x + icpt_power for x in lx]
    r2_power = r2(ra, pred_power)

    # Exponential / linear-in-time form (B = c - b*t): the shape an exponential decay
    # would leave in this base-level reservoir. Cannot capture the log-linear curve.
    slope_exp, icpt_exp = linreg(delay, ra)
    pred_exp = [slope_exp * t + icpt_exp for t in delay]
    r2_exp = r2(ra, pred_exp)

    fig, ax = plt.subplots(figsize=(7.5, 5.0))
    ax.scatter(delay, ra, color="#1b3a6b", zorder=3, s=42, label="engine retained_action")
    ax.plot(
        delay,
        pred_power,
        color="#c0392b",
        lw=2.0,
        label=f"power-law (linear in ln t)  r²={r2_power:.4f}, slope={slope_power:.3f}",
    )
    ax.plot(
        delay,
        pred_exp,
        color="#7f8c8d",
        lw=1.8,
        ls="--",
        label=f"exponential (linear in t)  r²={r2_exp:.4f}",
    )
    ax.set_xscale("log")
    ax.set_xlabel("retention interval (days, log scale)")
    ax.set_ylabel("retained_action  (base-level reservoir B)")
    ax.set_title(
        "Forgetting: power-law base-level decay\n"
        f"power-law fits far better than exponential "
        f"(r² {r2_power:.4f} vs {r2_exp:.4f})"
    )
    ax.legend(loc="upper right", framealpha=0.95)
    ax.grid(True, which="both", alpha=0.25)
    return save(fig, "fidelity_forgetting.png")


def plot_spacing():
    """Two panels: retention curves and the spaced-minus-clustered margin.

    Left: spaced/clustered/massed retained_action vs test day. Spaced and clustered
    share their final study day (25), so any separation between them is NOT recency.
    Right: the margin (spaced - clustered) crossing from negative to positive against
    a dashed zero line that is exactly what a recency-only model would predict.
    """
    header, rows = read_csv("spacing.csv")
    td = column(header, rows, "test_day")
    spaced = column(header, rows, "spaced")
    clustered = column(header, rows, "clustered")
    massed = column(header, rows, "massed")
    margin = column(header, rows, "margin_spaced_minus_clustered")

    # Crossover: first test day where the margin turns positive, refined by linear
    # interpolation between the bracketing samples (reported, drawn as a marker).
    cross_day = None
    for i in range(1, len(margin)):
        if margin[i - 1] < 0.0 <= margin[i]:
            x0, x1 = td[i - 1], td[i]
            y0, y1 = margin[i - 1], margin[i]
            cross_day = x0 + (0.0 - y0) * (x1 - x0) / (y1 - y0)
            break

    fig, (axl, axr) = plt.subplots(1, 2, figsize=(13.0, 5.2))

    axl.plot(td, spaced, color="#1b7837", lw=2.2, marker="o", ms=3, label="spaced [1,13,25]")
    axl.plot(
        td, clustered, color="#c0392b", lw=2.2, marker="s", ms=3, label="clustered [23,24,25]"
    )
    axl.plot(
        td, massed, color="#7f8c8d", lw=1.8, ls="--", marker="^", ms=3, label="massed [1,2,3]"
    )
    if cross_day is not None:
        axl.axvline(cross_day, color="#555555", lw=1.0, ls=":")
    axl.set_xlabel("test day (retention interval grows →)")
    axl.set_ylabel("retained_action at test")
    axl.set_title(
        "Spacing: retention vs retention interval\n"
        "spaced & clustered share final study day 25 (recency held constant)"
    )
    axl.legend(loc="upper right", framealpha=0.95)
    axl.grid(True, alpha=0.25)

    axr.axhline(
        0.0,
        color="#888888",
        lw=1.6,
        ls="--",
        label="recency-only model (Δ = 0)",
    )
    axr.plot(
        td,
        margin,
        color="#1b3a6b",
        lw=2.4,
        marker="o",
        ms=3.5,
        label="engine: spaced − clustered",
    )
    axr.fill_between(td, margin, 0.0, where=[m >= 0 for m in margin], color="#1b7837", alpha=0.12)
    axr.fill_between(td, margin, 0.0, where=[m < 0 for m in margin], color="#c0392b", alpha=0.12)
    if cross_day is not None:
        axr.axvline(cross_day, color="#555555", lw=1.0, ls=":")
        axr.scatter(
            [cross_day],
            [0.0],
            color="#000000",
            zorder=5,
            s=55,
            label=f"crossover ≈ day {cross_day:.1f}",
        )
    axr.set_xlabel("test day (retention interval grows →)")
    axr.set_ylabel("margin: spaced − clustered (retained_action)")
    axr.set_title(
        "Spacing effect that a recency-only model cannot produce\n"
        "margin crosses 0 from below: spaced overtakes clustered as RI grows"
    )
    axr.legend(loc="lower right", framealpha=0.95)
    axr.grid(True, alpha=0.25)

    fig.tight_layout()
    return save(fig, "fidelity_spacing.png"), cross_day


def plot_fan():
    """activation vs fan size, with a scaled 1/fan reference (Anderson fan effect)."""
    header, rows = read_csv("fan.csv")
    fan = column(header, rows, "fan")
    act = column(header, rows, "activation")

    # Reference anchored at fan=1: a scaled 1/N curve (Anderson fan effect shape).
    ref = [act[0] / k for k in fan]

    fig, ax = plt.subplots(figsize=(7.5, 5.0))
    ax.plot(fan, act, color="#1b3a6b", lw=2.2, marker="o", ms=6, label="engine activation")
    ax.plot(
        fan,
        ref,
        color="#c0392b",
        lw=1.8,
        ls="--",
        marker="x",
        ms=6,
        label="scaled 1/fan reference",
    )
    ax.set_xlabel("fan size (competing associations from the cue)")
    ax.set_ylabel("target activation (settled RWR)")
    ax.set_title(
        "Fan effect: activation divides across competitors\n"
        "engine tracks the 1/fan associative-strength reference"
    )
    ax.legend(loc="upper right", framealpha=0.95)
    ax.grid(True, alpha=0.25)
    return save(fig, "fidelity_fan.png")


def save(fig, name):
    os.makedirs(ASSET_DIR, exist_ok=True)
    path = os.path.join(ASSET_DIR, name)
    fig.savefig(path, dpi=140, bbox_inches="tight")
    plt.close(fig)
    print(path)
    return path


def main():
    plot_forgetting()
    _, cross_day = plot_spacing()
    plot_fan()
    if cross_day is not None:
        print(f"# spacing crossover (spaced - clustered turns positive) ≈ day {cross_day:.2f}")


if __name__ == "__main__":
    main()

"""Render the measured confirmation backlogs from backlog.csv (requires Matplotlib)."""
from collections import defaultdict
import csv
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import StrMethodFormatter

folder = Path(__file__).resolve().parent
series = defaultdict(list)
with (folder / "backlog.csv").open() as source:
    for row in csv.DictReader(source):
        series[row["trial"]].append((int(row["second"]), int(row["acknowledged_waiting_for_common"])))

fig, ax = plt.subplots(figsize=(8, 3.7), layout="constrained")
for trial, label, color in [
    ("round-a", "Round-robin destinations", "#2258a4"),
    ("ready-a", "Shared queue, 1,024 transactions per block", "#9e3e17"),
    ("ready-wide", "Shared queue, 2,048 limit, 2 VM workers", "#087b68"),
    ("ready-wide-4w", "Shared queue, 2,048 limit, 4 VM workers", "#7954a3"),
]:
    seconds, backlog = zip(*series[trial])
    ax.step(seconds, backlog, where="post", linewidth=1.8, label=label, color=color)
ax.axvspan(180, 225, color="#adb4bc", alpha=0.14)
ax.axvline(180, color="#59636e", linestyle="--", linewidth=0.9)
ax.text(183, 0.95, "Drain", transform=ax.get_xaxis_transform(), va="top", color="#444b55")
ax.set(xlim=(0, 225), ylim=(0, None), xlabel="Seconds from the first scheduled payment",
       ylabel="Acknowledged payments\nawaiting all four replicas")
ax.set_xticks([0, 30, 60, 90, 120, 150, 180, 210, 225])
ax.yaxis.set_major_formatter(StrMethodFormatter("{x:,.0f}"))
ax.grid(axis="y", color="#e1e5e9")
ax.spines[["right", "top"]].set_visible(False)
ax.legend(loc="upper left", facecolor="white", edgecolor="none", framealpha=0.96, fontsize=8)
fig.savefig(folder / "backlog.png", dpi=170, metadata={"Title": "Payment backlog on four disk-backed replicas"})

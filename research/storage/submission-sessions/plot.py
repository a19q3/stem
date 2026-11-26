"""Plot rolling common-finality throughput and acknowledged backlog from the archived payment IDs."""
import json
import lzma
from pathlib import Path
import sys

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import StrMethodFormatter

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "dispatch"))
from backlog import series

labels = {"fresh64": "Fresh · 64", "reuse64": "Reuse · 64", "reuse128": "Reuse · 128",
          "pause64": "Fresh · 64, pause on full"}
colors = ["#22649c", "#b34c16", "#008268", "#68499e"]
fig, (rates, queue) = plt.subplots(2, sharex=True, figsize=(8, 5.6), layout="constrained")
for (name, label), color in zip(labels.items(), colors, strict=True):
    with lzma.open(HERE / (name + ".json.xz"), "rt") as source:
        trial = json.load(source)
    data = list(series(trial))
    end = trial["load"]["seconds"]
    rates.plot(range(60, end+1), [(data[i][3]-data[i-60][3])/60 for i in range(60, end+1)],
               label=label, color=color, linewidth=1.7)
    queue.plot([r[0] for r in data], [r[4] for r in data], color=color, linewidth=1.7)
rates.axhline(3000, color="#53575e", linewidth=1, linestyle="--")
rates.text(222, 3025, "3,000 TPS", ha="right", color="#41454b")
rates.legend(ncol=2, frameon=False, loc="lower left", fontsize=8)
rates.set(ylabel="Rolling 60 s common TPS", ylim=(0, 3400))
queue.set(ylabel="Acknowledged payments\nawaiting four replicas", xlabel="Seconds since scheduled load began")
for ax in (rates, queue):
    ax.axvspan(end, data[-1][0], color="#8b969d", alpha=0.14)
    ax.yaxis.set_major_formatter(StrMethodFormatter("{x:,.0f}"))
    ax.spines[["top", "right"]].set_visible(False)
    ax.grid(axis="y", color="#dce2e6")
queue.set(xlim=(0, data[-1][0]), ylim=(0, None), xticks=[0, 30, 60, 90, 120, 150, 180, 210, 225])
queue.text(182, 0.92, "Drain", transform=queue.get_xaxis_transform(), color="#53575e")
fig.savefig(HERE / "throughput.png", dpi=165)

"""Render measured queue-time shares from the offline analysis."""
import json
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.ticker import PercentFormatter

HERE = Path(__file__).resolve().parent
PHASES = [
    ("candidate_verify", "Candidate verification", "#316795"),
    ("proposal", "Proposal construction", "#6a9dbc"),
    ("commit", "Certified commit", "#d88c46"),
    ("client_check", "Client admission check", "#4e8e68"),
    ("relay_check", "Relay admission check", "#a7c4a5"),
    ("other", "Other / unmeasured", "#cfcfd0"),
]


def render(trials):
    measured = [(name, t) for name, t in trials.items() if t["aggregate"]]
    measured.sort(key=lambda item: (item[1]["rate"], item[1]["vm_workers"],
                                    "candidate_parts" in item[1]["aggregate"], item[0] == "paired2"))
    fig, axes = plt.subplots(1, 2, figsize=(10.2, 3 + .38 * len(measured)), sharey=True)
    for axis, (key, title) in zip(axes, [("overlap_ns", "All submission waits"),
                                          ("long_wait_overlap_ns", "Waits of 100 ms or more")]):
        for row, (_, t) in enumerate(measured):
            times = t["aggregate"][key]
            total, left = sum(times.values()), 0
            for phase, name, color in PHASES:
                fraction = times.get(phase, 0) / total if total else 0
                axis.barh(row, fraction, left=left, height=.59, color=color,
                          label=name if row == 0 else None)
                if phase == "candidate_verify" and fraction > .1:
                    axis.text(left + fraction / 2, row, f"{fraction:.1%}", ha="center",
                              va="center", color="white", fontsize=10, fontweight="bold")
                left += fraction
        axis.set_xlim(0, 1)
        axis.set_xticks([0, .25, .5, .75, 1])
        axis.xaxis.set_major_formatter(PercentFormatter(1, decimals=0))
        axis.set_title(title, loc="left", fontsize=13, pad=12)
        axis.spines[["top", "right", "left"]].set_visible(False)
        axis.tick_params(axis="y", length=0, pad=7)
    suffix = {"parts2": " · detailed", "paired2": " · parallel receipts"}
    axes[0].set_yticks(range(len(measured)),
                       [f"{t['rate']:,}/s · {t['vm_workers']} " +
                        ("worker" if t["vm_workers"] == 1 else "workers") +
                        suffix.get(name, "") for name, t in measured])
    axes[0].invert_yaxis()
    fig.legend(*axes[0].get_legend_handles_labels(), loc="lower center", ncol=3,
               frameon=False, fontsize=9, bbox_to_anchor=(.55, .04))
    fig.suptitle("Share of cumulative submission queue time", x=.02, y=.96, ha="left",
                 fontsize=17, fontweight="bold")
    fig.subplots_adjust(left=.27, right=.98, top=.76, bottom=.26, wspace=.20)
    fig.savefig(HERE / "overlap.png", dpi=145)
    plt.close(fig)


if __name__ == "__main__":
    render(json.loads((HERE / "results.json").read_bytes()))

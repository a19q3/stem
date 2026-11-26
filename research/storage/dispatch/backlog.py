"""Recompute the number of acknowledged payments still awaiting common visibility."""
import argparse
from bisect import bisect_right
import csv
import json
import lzma
from pathlib import Path
import sys


def series(trial):
    times = trial["delivery_times"]
    writes = sorted(t for t, _ in times if t is not None)
    acks = sorted(t for _, t in times if t is not None)
    common, acknowledged_and_common = [], []
    for block in trial["blocks"]:
        observed = block["seen_us"]
        for index in block["positions"]:
            common.append(observed)
            ack = times[index][1]
            if ack is not None:
                acknowledged_and_common.append(max(ack, observed))
    common.sort()
    acknowledged_and_common.sort()
    load = trial["load"]
    for second in range(load["seconds"] + load["drain"] + 1):
        now = second * 1_000_000
        acknowledged = bisect_right(acks, now)
        yield (second, bisect_right(writes, now), acknowledged, bisect_right(common, now),
               acknowledged - bisect_right(acknowledged_and_common, now))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trials", type=Path, nargs="+")
    args = parser.parse_args()
    writer = csv.writer(sys.stdout, lineterminator="\n")
    writer.writerow(["trial", "second", "written", "acknowledged", "common", "acknowledged_waiting_for_common"])
    for path in args.trials:
        with lzma.open(path, "rt") as file:
            for row in series(json.load(file)):
                writer.writerow([path.name.removesuffix(".json.xz"), *row])

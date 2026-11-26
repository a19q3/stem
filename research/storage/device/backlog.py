"""Count signed payments still awaiting common visibility after a successful acknowledgement."""

import argparse
from bisect import bisect_right
import json
import lzma
from pathlib import Path


def backlog(path):
    with lzma.open(path, "rt") as f:
        report = json.load(f)["report.json"]
    seen = {txid: block["seen_us"] for block in report["blocks"] for txid in block["ids"]}
    ends = []
    starts = []
    ack_times = []
    writes = []
    for row in report["deliveries"]:
        if row["offered_us"] is not None:
            writes.append(row["offered_us"])
        ack = row["acknowledged_us"]
        if ack is None:
            continue
        ack_times.append(ack)
        end = seen.get(row["id"])
        if end is None or ack < end:
            starts.append(ack)
            if end is not None:
                ends.append(end)
    starts.sort()
    ends.sort()
    writes.sort()
    ack_times.sort()
    visible = sorted(seen.values())

    def waiting(t):
        return bisect_right(starts, t) - bisect_right(ends, t)

    peak, peak_at = max(((waiting(t), -t) for t in starts), default=(0, 0))
    duration = report["load"]["seconds"]
    cutoff = duration * 1_000_000
    step = 10 if duration < 180 else 60
    last_visible = max(visible, default=0)
    last = max(last_visible, cutoff, max(ack_times, default=0), max(writes, default=0))
    checkpoints = sorted({*range(step * 1_000_000, cutoff, step * 1_000_000), cutoff, last})
    rows = [{
        "time_us": t, "written": bisect_right(writes, t),
        "acknowledged": bisect_right(ack_times, t), "common_visible": bisect_right(visible, t),
        "acknowledged_awaiting_common_visibility": waiting(t),
    } for t in checkpoints]
    assert bisect_right(visible, cutoff) == report["summary"]["visible_during_offer"]
    return {
        "file": path.name, "offer_seconds": duration, "peak_acknowledged_waiting": peak,
        "first_peak_us": -peak_at, "waiting_at_offer_end": waiting(cutoff),
        "waiting_at_last_observation": waiting(last),
        "last_visible_after_offer_us": max(0, last_visible - cutoff),
        "checkpoints": rows,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archives", type=Path, nargs="+")
    print(json.dumps([backlog(path) for path in parser.parse_args().archives], indent=2))

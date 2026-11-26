"""Recompute disjoint common-visibility windows and the timing of RocksDB compaction."""

from bisect import bisect_right
import argparse
from collections import Counter
import json
import lzma
from pathlib import Path


def devices(sample):
    counts = Counter()
    for node in sample["resources"]:
        for line in node["device"]["io_stat"].splitlines():
            device, *fields = line.split()
            counts[device] += int(dict(field.split("=") for field in fields)["wbytes"])
    return counts


def compaction(sst, baseline_us, step_s, offer_s):
    rows = {}
    for log in sst["compaction_events"].values():
        for event in log:
            elapsed = (event["time_micros"] - baseline_us) // 1_000_000
            if elapsed < 0:
                label = "before offer"
            elif elapsed >= offer_s:
                label = "after offer"
            else:
                label = f"{elapsed // step_s * step_s} to {min(offer_s, (elapsed // step_s + 1) * step_s)} s"
            row = rows.setdefault(label, dict(started=0, finished=0, input_bytes=0, output_bytes=0,
                                              compaction_us=0, cpu_us=0, output_levels=Counter()))
            if event["event"] == "compaction_started":
                row["started"] += 1
                row["input_bytes"] += event["input_data_size"]
            else:
                assert event["event"] == "compaction_finished"
                row["finished"] += 1
                row["output_bytes"] += event["total_output_size"]
                row["compaction_us"] += event["compaction_time_micros"]
                row["cpu_us"] += event["compaction_time_cpu_micros"]
                row["output_levels"][event["output_level"]] += 1
    return rows


def windows(path, anchors):
    with lzma.open(path, "rt") as f:
        run = json.load(f)
    assert run["exit"] == 0
    report = run["report.json"]
    offered = {d["id"]: d["offered_us"] for d in report["deliveries"] if d["offered_us"] is not None}
    offers = sorted(offered.values())
    confirmations = sorted(
        (block["seen_us"], max(0, block["seen_us"] - offered[txid]))
        for block in report["blocks"] for txid in block["ids"]
    )
    visible = [at for at, _ in confirmations]
    errors = Counter(d["error"] for d in report["deliveries"] if d["error"])
    samples = report["samples"]
    times = [s["at_us"] for s in samples]
    previous = report["before"]
    previous_visible = previous_written = previous_boundary = 0
    rows = []
    seconds = report["load"]["seconds"]
    step = 600 if seconds >= 3600 else 60 if seconds >= 180 else 10
    for end in sorted({*range(step, seconds, step), seconds}):
        cutoff = end * 1_000_000
        index = bisect_right(times, cutoff) - 1
        count = bisect_right(visible, cutoff)
        written = bisect_right(offers, cutoff)
        assert index >= 0, "missing status sample before first interval"
        sample = samples[index]
        old, new = devices(previous), devices(sample)
        assert all(new[k] >= v for k, v in old.items())
        latencies = sorted(delay for _, delay in confirmations[previous_visible:count])
        p99 = latencies[(99 * len(latencies) + 99) // 100 - 1] if latencies else None
        rows.append(dict(offer_end_s=end, written=written-previous_written,
                         visible=count-previous_visible, common_tps=(count-previous_visible)/(end-previous_boundary),
                         confirmed_write_to_common_p99_us=p99,
                         sample_us=times[index], device_bytes=sum(new.values())-sum(old.values()),
                         largest_replica_mempool_at_sample=max(n["mempool_transactions"] for n in sample["statuses"])))
        previous, previous_boundary = sample, end
        previous_written, previous_visible = written, count
    assert previous_visible == report["summary"]["visible_during_offer"]
    result = dict(file=path.name, offer_windows=rows, observation_errors=len(report["observation_errors"]),
                  longest_sample_gap_us=max(t-p for p, t in zip([0, *times], times)), delivery_errors=dict(errors))
    anchor = run.get("before_file_mtime_ns")
    external = anchors.get(path.name.removesuffix(".json.xz"))
    if external:
        assert ("/" + external["fixture"] + "\n") in run["log"]
        anchor = external["before_file_mtime_ns"]
    if anchor:
        result["before_file_mtime_ns"] = anchor
        result["compaction_by_approximate_offer_time"] = compaction(run["sst"], anchor//1000, step, seconds)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clock-anchors", type=Path)
    parser.add_argument("archives", type=Path, nargs="+")
    args = parser.parse_args()
    anchors = json.loads(args.clock_anchors.read_bytes()) if args.clock_anchors else {}
    print(json.dumps([windows(path, anchors) for path in args.archives], indent=2))

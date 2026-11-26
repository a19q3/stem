"""Measure timing stages when the payment driver and validators share a host clock."""

import argparse
import json
import lzma
from pathlib import Path

STAGES = ("write_to_ack_us", "ack_to_header_us", "header_to_common_us", "write_to_common_us")


def quantiles(values):
    ordered = sorted(values)
    if not ordered:
        return None
    return {f"p{p}": ordered[(p * len(ordered) + 99) // 100 - 1] for p in (50, 95, 99)}


def offset(sample):
    return sample["unix_us"] - (sample["before_us"] + sample["after_us"]) / 2


def stage_summary(rows):
    summary = {"acknowledged_and_visible": len(rows)}
    for i, label in enumerate(STAGES):
        summary[label] = quantiles(row[i] for row in rows)
    return summary


def analyze(trial, legacy_start_us=None):
    clocks = trial.get("driver_clock")
    if clocks is None and legacy_start_us is None:
        raise ValueError("the older archive needs the recorded before.json mtime in microseconds")
    start = offset(clocks["start"]) if clocks else legacy_start_us
    limit = trial["load"]["seconds"] * 1_000_000
    minutes = [0] * ((limit + 59_999_999) // 60_000_000)
    endpoints = [{"written": 0, "acknowledged": 0, "in_window": 0, "confirmed": 0} for _ in trial["external_endpoints"]]
    for index, (write, ack) in enumerate(trial["delivery_times"]):
        counts = endpoints[index % len(endpoints)]
        counts["written"] += int(write is not None)
        counts["acknowledged"] += int(ack is not None)
    window, drain, visible_latencies = [], [], []
    block_sizes, header_times = [], []
    empty_blocks = 0
    late_ack = 0
    for block in trial["blocks"]:
        raw = bytes.fromhex(block["header"])
        # Native Header starts with uint64 height and uint64 Unix time in milliseconds.
        height = int.from_bytes(raw[:8], "little")
        header_us = int.from_bytes(raw[8:16], "little") * 1000 - start
        assert height <= trial["agreed_height"]
        common = block["seen_us"]
        count = len(block["positions"])
        if common <= limit:
            minutes[min(common // 60_000_000, len(minutes) - 1)] += count
            header_times.append(header_us)
            if count:
                block_sizes.append(count)
            else:
                empty_blocks += 1
        rows = window if common <= limit else drain
        for index in block["positions"]:
            counts = endpoints[index % len(endpoints)]
            counts["confirmed"] += 1
            counts["in_window"] += int(common <= limit)
            write, ack = trial["delivery_times"][index]
            assert write is not None
            visible_latencies.append(common - write)
            if ack is None:
                continue
            late_ack += int(header_us < ack)
            rows.append((ack - write, round(header_us - ack), round(common - header_us), common - write))

    load, summary = trial["load"], trial["summary"]
    count_limit = load.get("block_transactions", 1024)
    assert sum(minutes) == summary["visible_during_offer"]
    return {
        "write_to_common_us": quantiles(visible_latencies),
        "per_submission_endpoint": dict(zip(trial["external_endpoints"], endpoints)),
        "workload_id": trial["workload_id"], "certified_height": trial["agreed_height"],
        "cadence_ms": load["cadence_ms"], "block_transactions": count_limit, "block_cycles": load["block_cycles"],
        "senders": load["senders"], "per_endpoint_pools": load.get("shard_senders", False),
        "scheduled": summary["planned"], "written": summary["offered"], "acknowledged": summary["admitted"],
        "dropped_before_write": summary["client_drops"], "in_window": summary["visible_during_offer"],
        "confirmed": summary["visible"], "window_tps": summary["visibility_tps"], "in_window_by_minute": minutes,
        "after_window": summary["visible"] - summary["visible_during_offer"],
        "window_nonempty_blocks": len(block_sizes), "window_empty_blocks": empty_blocks,
        "window_blocks_at_count_limit": block_sizes.count(count_limit), "window_block_sizes": quantiles(block_sizes),
        "window_header_gap_us": quantiles(round(b - a) for a, b in zip(header_times, header_times[1:])),
        "ack_after_header": late_ack,
        "clock_offset_change_us": round(offset(clocks["end"]) - start, 1) if clocks else None,
        "clock_sampling_span_us": {k: s["after_us"] - s["before_us"] for k, s in clocks.items()} if clocks else None,
        "all": stage_summary(window + drain), "window": stage_summary(window), "drain": stage_summary(drain),
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trials", type=Path, nargs="+")
    parser.add_argument("--legacy-start-unix-us", type=int, help="approximate epoch for the uncalibrated baseline")
    args = parser.parse_args()
    results = {}
    for path in args.trials:
        with lzma.open(path, "rt") as source:
            results[path.name] = analyze(json.load(source), args.legacy_start_unix_us)
    print(json.dumps(results, indent=2))

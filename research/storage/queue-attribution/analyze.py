"""Intersect each observed submit wait with disjoint spans on the same replica's monotonic clock."""
import argparse
from bisect import bisect_left, bisect_right
from collections import Counter
import hashlib
import json
import lzma
from pathlib import Path
import struct
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "dispatch"))
from backlog import series
from collect import digest

RECORD = struct.Struct("<cQQ")
NAMES = {b"Q": "queue", b"V": "candidate_verify", b"P": "proposal", b"C": "commit",
         b"A": "client_check", b"R": "relay_check"}
PARTS = {b"d": "decode", b"b": "enter_branch", b"e": "stage_transactions",
         b"f": "finish_block", b"w": "return_from_worker"}


class Coverage:
    def __init__(self, intervals):
        self.starts = []
        self.ends = []
        self.totals = [0]
        for start, end in intervals:
            self.starts.append(start)
            self.ends.append(end)
            self.totals.append(self.totals[-1] + end - start)

    def through(self, instant):
        full = bisect_right(self.ends, instant)
        total = self.totals[full]
        if full < len(self.starts) and self.starts[full] < instant:
            total += instant - self.starts[full]
        return total

    def contains(self, instant):
        index = bisect_right(self.starts, instant) - 1
        return index >= 0 and instant < self.ends[index]


def ordinals(snapshot):
    return {
        "queue": sum(snapshot["queue"]["buckets"]),
        "admission": snapshot["checks"],
        **{name: sum(metric["buckets"]) for name, metric in snapshot["execution"].items()},
    }


def spans(path, expected, before, after, parts=None):
    with lzma.open(path, "rb") as src:
        data = src.read()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"uncompressed trace hash mismatch for {path}")
    phases = {name: [] for name in NAMES.values()}
    ordinal = Counter()
    low, high = ordinals(before), ordinals(after)
    last_work_end = 0
    last_record_end = 0
    pending = []
    split_candidates = None
    for tag, start, end in RECORD.iter_unpack(data):
        if start > end or end < last_record_end:
            raise ValueError("node event clock regressed")
        last_record_end = end
        if tag in PARTS:
            pending.append((tag, start, end))
            continue
        name = NAMES[tag]
        if name == "candidate_verify":
            if split_candidates is None:
                split_candidates = bool(pending)
            if split_candidates != bool(pending):
                raise ValueError("candidate subdivision disappeared or appeared partway through the run")
        if pending:
            if (name != "candidate_verify" or [p[0] for p in pending] != list(PARTS)
                    or pending[0][1] != start or pending[-1][2] != end
                    or any(a[2] != b[1] for a, b in zip(pending, pending[1:]))):
                raise ValueError("candidate parts do not partition the verification interval")
        if name != "queue":
            if start < last_work_end:
                raise ValueError("replica phase intervals overlap")
            last_work_end = end
        key = "admission" if tag in (b"A", b"R") else name
        index = ordinal[key]
        ordinal[key] += 1
        if low[key] <= index < high[key]:
            phases[name].append((start, end))
            if parts is not None:
                for part, a, b in pending:
                    parts.setdefault(PARTS[part], []).append((a, b))
        pending.clear()
    if pending:
        raise ValueError("incomplete candidate subdivision at end of trace")
    if any(ordinal[k] < n for k, n in high.items()):
        raise ValueError("a trace ends before the final counter snapshot")
    actual = {k: sum((b-a)//1000 for a, b in rows) for k, rows in phases.items()}
    expected_us = {"queue": after["queue"]["elapsed_us"] - before["queue"]["elapsed_us"],
                   **{k: v["elapsed_us"]-before["execution"][k]["elapsed_us"] for k, v in after["execution"].items()}}
    if any(actual[k] != v for k, v in expected_us.items()):
        raise ValueError("event times disagree with the node histograms")
    if actual["client_check"] + actual["relay_check"] != after["verification_us"] - before["verification_us"]:
        raise ValueError("admission check duration differs from the node counter")
    return phases


def intersect(phases):
    waits = phases["queue"]
    coverage = {name: Coverage(rows) for name, rows in phases.items() if name != "queue"}
    totals = {name: 0 for name in (*coverage, "other")}
    arrivals = dict(totals)
    tails = dict(totals)
    tail_count = 0
    for start, end in waits:
        remaining = end - start
        arrival = "other"
        is_tail = remaining >= 100_000_000
        tail_count += is_tail
        for name, measured in coverage.items():
            elapsed = measured.through(end) - measured.through(start)
            remaining -= elapsed
            totals[name] += elapsed
            if is_tail:
                tails[name] += elapsed
            if measured.contains(start):
                arrival = name
        if remaining < 0:
            raise ValueError("phase overlap exceeds the measured queue wait")
        totals["other"] += remaining
        if is_tail:
            tails["other"] += remaining
        arrivals[arrival] += 1
    if sum(totals.values()) != sum(end-start for start, end in waits):
        raise ValueError("accumulated wait attribution differs from the observed waits")
    durations = sorted(end-start for start, end in waits)
    return {"requests": len(waits), "overlap_ns": totals, "arrival_phase": arrivals,
            "waits_at_least_100ms": tail_count, "long_wait_overlap_ns": tails,
            "phase_ns": {name: measured.totals[-1] for name, measured in coverage.items()},
            "phase_calls": {name: len(measured.starts) for name, measured in coverage.items()},
            "queue_p99_ns": durations[(99*len(durations)+99)//100-1] if durations else None}


def analyze(directory):
    meta = json.loads((directory / "counters.json").read_bytes())
    archive = directory / "payments.json.xz"
    if digest(archive) != meta["payment_sha256"]:
        raise ValueError("payment archive hash mismatch")
    with lzma.open(archive, "rt") as f:
        trial = json.load(f)
    routes = Counter(r for r in trial["delivery_routes"] if r is not None)
    for key in ("before", "after"):
        if [s["member"] for s in meta[key]] != [0, 1, 2, 3]:
            raise ValueError("expected four ordered validator snapshots")
    expected_names = {f"node-{i}.trace" for i in range(4)}
    if meta["trace_hashes"] and set(meta["trace_hashes"]) != expected_names:
        raise ValueError("expected one trace per validator")
    nodes = []
    for i, (before, after) in enumerate(zip(meta["before"], meta["after"], strict=True)):
        count = sum(after["queue"]["buckets"]) - sum(before["queue"]["buckets"])
        if routes[i] != count:
            raise ValueError("driver socket destinations disagree with node request counts")
        name = f"node-{i}.trace"
        if name in meta["trace_hashes"]:
            parts = {}
            phases = spans(directory / (name + ".xz"), meta["trace_hashes"][name], before, after, parts)
            node = intersect(phases)
            if parts:
                child = intersect({**parts, "queue": phases["queue"]})
                node["candidate_parts"] = {}
                for metric in ("overlap_ns", "long_wait_overlap_ns", "phase_ns"):
                    values = {p: child[metric][p] for p in PARTS.values()}
                    if sum(values.values()) != node[metric]["candidate_verify"]:
                        raise ValueError("candidate subphase time differs from the parent")
                    node["candidate_parts"][metric] = values
            nodes.append(node)
    load = trial["load"]
    errors = meta["driver_errors"]
    if sum(errors.values()) != len(trial["delivery_times"]) - trial["summary"]["admitted"]:
        raise ValueError("driver error counts disagree with the payment archive")
    rows = list(series(trial))
    block_counts = [len(b["positions"]) for b in trial["blocks"] if b["positions"]]
    seconds = load["seconds"]
    written = sorted(t for t, _ in trial["delivery_times"] if t is not None)
    buckets = [bisect_left(written, (s+1)*1_000_000) - bisect_left(written, s*1_000_000) for s in range(seconds)]
    lateness = sorted(written_at - (i * 1_000_000 // load["rate"])
                      for i, (written_at, _) in enumerate(trial["delivery_times"]) if written_at is not None)
    if not lateness or lateness[0] < 0:
        raise ValueError("missing writes or write timestamp earlier than its scheduled arrival")
    aggregate = {}
    if nodes:
        for key in ("overlap_ns", "arrival_phase", "long_wait_overlap_ns", "phase_ns", "phase_calls"):
            aggregate[key] = dict(sum((Counter(n[key]) for n in nodes), Counter()))
        aggregate["waits_at_least_100ms"] = sum(n["waits_at_least_100ms"] for n in nodes)
        aggregate["requests"] = sum(n["requests"] for n in nodes)
        if any("candidate_parts" in n for n in nodes):
            aggregate["candidate_parts"] = {
                key: dict(sum((Counter(n["candidate_parts"][key]) for n in nodes), Counter()))
                for key in ("overlap_ns", "long_wait_overlap_ns", "phase_ns")
            }
    return {
        "workload": trial["workload_id"], "genesis": meta["genesis"]["genesis_id"],
        "vm_workers": load["vm_workers"], "rate": load["rate"], "seconds": seconds,
        "written_per_second": buckets,
        "schedule_to_write_us": {f"p{p}": lateness[(p*len(lateness)+99)//100-1] for p in (50, 95, 99, 100)},
        "window_writes": rows[seconds][1], "window_acks": rows[seconds][2], "window_common": rows[seconds][3],
        "window_tps": rows[seconds][3] / seconds, "end_common": rows[-1][3], "end_backlog": rows[-1][4],
        "common_each_minute": [rows[s][3] - rows[s-60][3] for s in range(60, seconds+1, 60)],
        "payment_blocks": len(block_counts),
        "full_payment_blocks": sum(n == load["block_transactions"] for n in block_counts),
        "blocks_at_least_256_payments": sum(n >= 256 for n in block_counts),
        "summary": trial["summary"], "driver_errors": errors,
        "storage": dict(sum((Counter(delta) for delta in meta["storage_deltas"]), Counter())),
        "nodes": nodes, "aggregate": aggregate,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directories", type=Path, nargs="+")
    args = parser.parse_args()
    print(json.dumps({d.name: analyze(d) for d in args.directories}, indent=2))

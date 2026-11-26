"""Reconstruct concurrent payment submissions on the load driver's clock."""
import argparse
from bisect import bisect_right
from collections import Counter
import json
import lzma
from pathlib import Path


def distribution(values):
    return {str(n): count for n, count in sorted(Counter(values).items())}


def analyze(trial):
    load = trial["load"]
    times = trial["delivery_times"]
    peers = len(trial["external_endpoints"])
    routes = trial.get("delivery_routes")
    ready = load.get("ready_senders", False)
    if ready != (routes is not None) or (ready and len(routes) != len(times)):
        raise ValueError("missing or unexpected endpoint assignments")
    starts = [[] for _ in range(peers)]
    ends = [[] for _ in range(peers)]
    unsuccessful = trial.get("unacknowledged_responses", [])
    responses = dict(unsuccessful)
    if len(responses) != len(unsuccessful) or any(
            not 0 <= i < len(times) or times[i][0] is None or times[i][1] is not None for i in responses):
        raise ValueError("unsuccessful reply does not identify a distinct unacknowledged write")
    acknowledgements = [0] * peers
    unacknowledged = 0
    unresolved = 0
    for i, (write, ack) in enumerate(times):
        if write is None:
            if ack is not None or (ready and routes[i] is not None):
                raise ValueError("unwritten payment has a reply or endpoint")
            continue
        endpoint = routes[i] if ready else i % peers
        if not 0 <= endpoint < peers:
            raise ValueError("invalid submission endpoint")
        unacknowledged += int(ack is None)
        acknowledgements[endpoint] += int(ack is not None)
        end = responses.get(i, ack)
        if end is None:
            unresolved += 1
            continue
        if end < write:
            raise ValueError("reply precedes write")
        starts[endpoint].append(write)
        ends[endpoint].append(end)
    for series in starts + ends:
        series.sort()

    def count_at(t):
        return [bisect_right(s, t) - bisect_right(e, t) for s, e in zip(starts, ends)]

    def pressure(timestamps):
        totals, peaks, least = [], [], []
        for t in timestamps:
            counts = count_at(t)
            totals.append(sum(counts))
            peaks.append(max(counts))
            least.append(min(counts))
        return {"observations": len(totals), "pending_all": distribution(totals),
                "pending_busiest_endpoint": distribution(peaks), "pending_quietest_endpoint": distribution(least)}

    result = {
        "workload_id": trial["workload_id"], "senders": load["senders"], "ready_senders": ready,
        "writes_missing_successful_replies": unacknowledged, "writes_without_end_observation": unresolved,
        "response_worker_seconds": round(sum(sum(e) - sum(s) for s, e in zip(starts, ends)) / 1_000_000, 6),
        "max_open_per_endpoint": [max((i + 1 - bisect_right(e, t) for i, t in enumerate(s)), default=0)
                                  for s, e in zip(starts, ends)],
        "acked_per_endpoint": acknowledgements,
        "all_unwritten_at_planned_time": pressure(i * 1_000_000 // load["rate"]
                                                 for i, (write, _) in enumerate(times) if write is None),
    }
    cap = load["senders"] // peers if ready or load.get("shard_senders", False) else load["senders"]
    if max(result["max_open_per_endpoint"]) > cap:
        raise ValueError("observed concurrent requests exceed the sender pool")
    if "queue_full_us" in trial:
        full = trial["queue_full_us"]
        if len(full) != len(times):
            raise ValueError("incomplete queue-full trace")
        for i, t in enumerate(full):
            if t is not None and (times[i][0] is not None or t < i * 1_000_000 // load["rate"]):
                raise ValueError("queue-full event inconsistent with dispatch")
        result["queue_full_at_observed_time"] = pressure(t for t in full if t is not None)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trials", type=Path, nargs="+")
    args = parser.parse_args()
    result = {}
    for path in args.trials:
        with lzma.open(path, "rt") as source:
            result[path.name] = analyze(json.load(source))
    print(json.dumps(result, indent=2))

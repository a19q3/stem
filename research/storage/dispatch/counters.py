"""Extract replica and driver time counters from a raw funded-payment measurement."""
import argparse
from collections import Counter
import json
from pathlib import Path


def timing(before, after):
    return {
        "count": sum(after["buckets"]) - sum(before["buckets"]),
        "elapsed_us": after["elapsed_us"] - before["elapsed_us"],
    }


def summarize(report):
    peers = []
    snapshots = zip(report["before"]["statuses"], report["after"]["statuses"], strict=True)
    for index, (before, after) in enumerate(snapshots):
        if before["member"] != after["member"]:
            raise ValueError("validator identity changed between snapshots")
        samples = [s["statuses"][index] for s in report["samples"]]
        if not samples or any(s["member"] != after["member"] for s in samples):
            raise ValueError("missing or reordered validator samples")
        admission = {}
        for key in ("client_queue", "client_service"):
            admission[key] = timing(before["admission"][key], after["admission"][key])
        for key in ("client_requests", "relayed_requests", "checks", "verification_us"):
            admission[key] = after["admission"][key] - before["admission"][key]
        execution = {k: timing(before["execution"][k], after["execution"][k]) for k in before["execution"]}
        maximum = {k: max(s[k] for s in samples) for k in ("mempool_bytes", "mempool_transactions")}
        peers.append({"member": after["member"], "admission": admission, "execution": execution,
                      "sampled_max": maximum, "transport_drops": after["transport_drops"] - before["transport_drops"]})

    stages = [
        ("started_us", "connected_us"), ("connected_us", "offered_us"),
        ("offered_us", "response_us"), ("response_us", "acknowledged_us"),
    ]
    durations = {f"{a}_to_{b}": 0 for a, b in stages}
    errors = Counter()
    for row in report["deliveries"]:
        for left, right in stages:
            if row[right] is not None:
                if row[left] is None or row[right] < row[left]:
                    raise ValueError("submission stages are out of order")
                durations[f"{left}_to_{right}"] += row[right] - row[left]
        if row["error"]:
            errors[row["error"]] += 1

    return {"workload_id": report["workload_id"], "certified_height": report["agreed_height"],
            "driver_total_us": durations, "client_errors": dict(errors), "replica_counters": peers}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    with args.report.open() as source:
        print(json.dumps(summarize(json.load(source)), indent=2))

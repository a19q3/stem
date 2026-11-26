"""Recompute throughput, queue time, CPU and storage for the recorded payment trials."""
import argparse
from bisect import bisect_left, bisect_right
import hashlib
import json
import lzma
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "dispatch"))
sys.path.insert(0, str(HERE.parent / "proposal-cadence" / "nvme"))
from backlog import series
from device import summarize as device_bounds


def read(path):
    with lzma.open(path, "rt") as source:
        return json.load(source)


def cpu_bounds(cpu, start, end):
    samples = cpu["samples"]
    starts = [s for s, _, _ in samples]
    ends = [e for _, e, _ in samples]
    if any(a > b for a, b in zip(ends, starts[1:])):
        raise ValueError("process sampling clock regressed")

    def bracket(t):
        before, after = bisect_right(ends, t) - 1, bisect_left(starts, t)
        if before < 0 or after == len(samples):
            raise ValueError("CPU samples do not cover the measurement")
        return samples[before][2], samples[after][2]

    lo1, hi1 = bracket(start)
    lo2, hi2 = bracket(end)
    result = {}
    for key in lo1:
        lower, upper = lo2[key]-hi1[key], hi2[key]-lo1[key]
        if not 0 <= lower <= upper:
            raise ValueError("process CPU counter regressed")
        result[key] = [lower/cpu["ticks_per_second"], upper/cpu["ticks_per_second"]]
    result["validators"] = [sum(values[i] for name, values in result.items() if name != "driver")
                            for i in range(2)]
    return result


def summarize(trial, measurements):
    load = trial["load"]
    duration = load["seconds"]
    rows = list(series(trial))
    _, written, acknowledged, common, backlog = rows[duration]
    cutoff = duration*1_000_000
    assert common == trial["summary"]["visible_during_offer"]
    connections = trial["submission_connections"]
    assert len(connections) == len(trial["delivery_times"])
    reused = sum(item is True for item in connections)
    assert reused == trial["summary"]["reused_connections"]
    fresh = sum(item is False for item in connections)
    stats = measurements["counters"]
    if stats["workload_id"] != trial["workload_id"] or stats["certified_height"] != trial["agreed_height"]:
        raise ValueError("counter snapshots do not match the signed-payment archive")
    nodes = stats["replica_counters"]
    errors = stats["client_errors"]
    pauses = trial.get("full_mempool_pauses", [])
    unacknowledged = dict(trial.get("unacknowledged_responses", []))
    refusal_times = sorted(unacknowledged.values())
    if len(refusal_times) != errors.get("mempool capacity exceeded", 0):
        raise ValueError("this study assumes every unsuccessful reply names the full mempool")
    if load.get("pause_on_full", False):
        assert len(pauses) == errors.get("mempool capacity exceeded", 0)
        assert len({position for position, _, _ in pauses}) == len(pauses)
        deadline = cutoff + (load["drain"]*1_000_000 if load["complete_volume"] else 0)
        for position, began, ended in pauses:
            assert unacknowledged[position] <= began <= ended
            assert ended - began >= min(load["cadence_ms"]*1000, max(0, deadline-began))
    else:
        assert not pauses
    stages = stats["driver_total_us"]
    peers = [n["admission"] for n in nodes]
    replies = sum(n["client_queue"]["count"] for n in peers)
    observed_replies = sum(ack is not None for _, ack in trial["delivery_times"]) + len(trial.get("unacknowledged_responses", []))
    if replies != observed_replies:
        raise ValueError("server samples and driver reply counts differ")
    seconds_queued = sum(n["client_queue"]["elapsed_us"] for n in peers)/1e6
    acked_common = [b["seen_us"] - trial["delivery_times"][i][0] for b in trial["blocks"] for i in b["positions"]
                    if trial["delivery_times"][i][1] is not None]
    acked_common.sort()
    clock = trial["driver_clock"]["start"]
    unix_start = round(clock["unix_us"] - (clock["before_us"] + clock["after_us"])/2)
    end = unix_start + cutoff
    drain_end = end + load["drain"]*1_000_000
    files = {}
    for t0, t1, phase in ((unix_start, end, "offer"), (end, drain_end, "drain")):
        files[phase] = {kind: {"bytes": 0, "files": 0} for kind in ("flush", "compaction", "recovery", "other")}
        for node, created, job, number, kind, size in measurements["sst"]:
            if t0 < created <= t1:
                files[phase][kind]["bytes"] += size
                files[phase][kind]["files"] += 1
    rolling = [(rows[i][3] - rows[i-60][3])/60 for i in range(60, duration+1)]
    minutes = []
    for end_second in range(60, duration+1, 60):
        start_second = end_second - 60
        _, writes0, acks0, common0, _ = rows[start_second]
        _, writes1, acks1, common1, backlog1 = rows[end_second]
        rejected = bisect_right(refusal_times, end_second*1_000_000) - bisect_right(refusal_times, start_second*1_000_000)
        minutes.append({"written": writes1-writes0, "acknowledged": acks1-acks0, "common": common1-common0,
                        "refused": rejected, "backlog_at_end": backlog1})
    bounds = device_bounds(trial, measurements["device"])
    return {
        "workload": trial["workload_id"], "genesis": measurements["genesis_id"], "window_tps": common/duration,
        "rolling60_tps": [min(rolling), max(rolling)],
        "minutes": minutes, "first_full_reply_us": refusal_times[0] if refusal_times else None,
        "window_written": written, "window_acknowledged": acknowledged, "window_common": common,
        "window_backlog": backlog, "end_common": rows[-1][3], "end_acknowledged": rows[-1][2],
        "end_backlog": rows[-1][4], "peak_backlog": max(r[4] for r in rows),
        "write_to_common_p99_us": acked_common[(99*len(acked_common)+99)//100-1],
        "fresh_connections": fresh, "reused_requests": reused, "client_errors": errors,
        "full_mempool_pauses": len(pauses), "sender_pause_seconds": sum(end-begin for _,begin,end in pauses)/1e6,
        "mean_connect_us": stages["started_us_to_connected_us"]/(fresh+reused),
        "mean_write_to_reply_us": stages["offered_us_to_response_us"]/replies,
        "server_queue_seconds": seconds_queued,
        "queue_fraction_of_reply_wait": seconds_queued/(stages["offered_us_to_response_us"]/1e6),
        "server_service_seconds": sum(n["client_service"]["elapsed_us"] for n in peers)/1e6,
        "verified_admissions": sum(n["checks"] for n in peers),
        "admission_verification_seconds": sum(n["verification_us"] for n in peers)/1e6,
        "per_node_execution_seconds": {k:sum(n["execution"][k]["elapsed_us"] for n in nodes)/len(nodes)/1e6
                                       for k in nodes[0]["execution"]},
        "cpu_offer_seconds": cpu_bounds(measurements["cpu"], unix_start, end),
        "cpu_offer_and_drain_seconds": cpu_bounds(measurements["cpu"], unix_start, drain_end),
        "device_write_bytes": [sum(v[i] for v in bounds["total_wbytes_bounds"].values()) for i in range(2)],
        "sst_creation": files, "rocksdb_delta": measurements["rocksdb_delta"],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", nargs="?", type=Path, default=HERE)
    args = parser.parse_args()
    results = {}
    for name in ("fresh64", "reuse64", "reuse128", "pause64"):
        measurements = read(args.directory / (name + "-observations.json.xz"))[name]
        archive = args.directory / (name + ".json.xz")
        with archive.open("rb") as raw:
            if hashlib.file_digest(raw, "sha256").hexdigest() != measurements["archive_sha256"]:
                raise ValueError(f"{archive.name} differs from the collected archive")
        results[name] = summarize(read(archive), measurements)
    if len({(row["workload"], row["genesis"]["genesis_id"]) for row in results.values()}) != 1:
        raise ValueError("the trials do not share a signed workload and genesis")
    print(json.dumps(results, indent=2))

"""Recompute attributed writes and common visibility from a native-load archive."""

import argparse
import json
import lzma
from pathlib import Path


def device_writes(report):
    def bytes_by_device(snapshot):
        rows = {}
        for line in snapshot.splitlines():
            device, *counters = line.split()
            assert device not in rows
            values = dict(field.split("=", 1) for field in counters)
            rows[device] = int(values["wbytes"])
        return rows

    scopes = set()
    writes = []
    for before, after in zip(report["before"]["resources"], report["after"]["resources"], strict=True):
        old, new = before["device"], after["device"]
        assert old["scope"] == new["scope"] and old["scope"] not in scopes
        scopes.add(old["scope"])
        old, new = bytes_by_device(old["io_stat"]), bytes_by_device(new["io_stat"])
        assert old.keys() <= new.keys()
        delta = {dev: count - old.get(dev, 0) for dev, count in new.items()}
        assert all(count >= 0 for count in delta.values()) and sum(delta.values()) > 0
        writes.append(delta)
    assert len(scopes) == report["load"]["replicas"]
    return writes


def summarize(path):
    with lzma.open(path, "rt") as f:
        run = json.load(f)
    assert run["exit"] == 0
    r = run["report.json"]
    load, summary = r["load"], r["summary"]
    deliveries = r["deliveries"]
    written = {d["id"] for d in deliveries if d["offered_us"] is not None}
    admitted = {d["id"] for d in deliveries if d["acknowledged_us"] is not None}
    included = [i for block in r["blocks"] for i in block["ids"]]
    assert len(deliveries) == load["rate"] * load["seconds"] == summary["planned"]
    assert len(included) == len(set(included)) == summary["visible"]
    assert len(written) == summary["offered"] and len(admitted) == summary["admitted"]
    assert set(included) <= written and admitted <= written
    assert r["finality"] and r["header"] and run["history-index-proof.json"]["header"]
    compaction_jobs = set()
    for log, events in run["sst"]["logs"].items():
        compaction_jobs.update(
            (log, e["job"]) for e in events if e["kind"] == "compaction"
        )
    writes = {}
    for kind in ["finality", "journal"]:
        for field in ["serialized", "sync_microseconds", "writes"]:

            def total(state):
                records = [row["writes"][kind] for row in r[state]["storage"]]
                return sum(
                    (x["batch_bytes"] if field == "serialized" else x)[field]
                    for x in records
                )

            writes[kind + "_" + field] = total("after") - total("before")

    def resource_delta(name):
        return sum(x[name] for x in r["after"]["resources"]) - sum(
            x[name] for x in r["before"]["resources"]
        )

    replicas = load["replicas"]
    visible = summary["visible"]
    result = dict(
        file=path.name,
        replicas=replicas,
        seconds=load["seconds"],
        planned=summary["planned"],
        written=len(written),
        admitted=len(admitted),
        visible=visible,
        drops=summary["client_drops"],
        visible_tps=summary["visibility_tps"],
        p99_common_us=summary["latency_us"]["p99"],
        p99_client_queue_us=summary["submission_us"]["queue"]["p99"],
        p99_client_response_us=summary["submission_us"]["response"]["p99"],
        cpu_seconds=resource_delta("cpu_ticks") / 100,
        peak_sampled_node_rss_kib=max(
            x["resident_kib"]
            for s in [r["before"], *r["samples"], r["after"]]
            for x in s["resources"]
        ),
        process_write_B_per_confirmed_replica=resource_delta("write_bytes")
        / visible
        / replicas,
        wal_bytes=sum(
            a["rocksdb"]["wal_bytes"] - b["rocksdb"]["wal_bytes"]
            for a, b in zip(r["after"]["storage"], r["before"]["storage"])
        ),
        compaction_jobs_creating_files=len(compaction_jobs),
        sst=run["sst"]["totals"],
        batch=writes,
        agreed_height=r["agreed_height"],
        workload_sha256=run["workload.bin_sha256"],
        genesis_sha256=run["genesis.json_sha256"],
        observer_errors=len(r["observation_errors"]),
    )
    if run.get("scratch_mount") and r["before"]["resources"][0].get("device"):
        by_node = device_writes(r)
        total = sum(sum(node.values()) for node in by_node)
        namespaces = {}
        for before, after in zip(r["before"]["storage"], r["after"]["storage"], strict=True):
            for phase, values in after["writes"].items():
                previous = before["writes"][phase]["batch_bytes"]
                for key, value in values["batch_bytes"].items():
                    namespaces[key] = namespaces.get(key, 0) + value - previous[key]
        assert sum(v for k, v in namespaces.items() if k != "serialized") == namespaces["serialized"]
        result["device"] = {
            "mount": run["scratch_mount"], "bytes_by_node_and_device": by_node,
            "total_bytes": total, "per_confirmed_replica": total / (visible * replicas),
            "logical_bytes_by_namespace": namespaces,
            "device_over_serialized_batch_bytes": total / namespaces["serialized"],
        }
    for kind in ["candidate_verify", "proposal", "commit"]:

        def elapsed(state):
            return sum(x["execution"][kind]["elapsed_us"] for x in r[state]["statuses"])

        def count(state):
            return sum(
                sum(x["execution"][kind]["buckets"]) for x in r[state]["statuses"]
            )

        result[kind + "_mean_us"] = (elapsed("after") - elapsed("before")) / (
            count("after") - count("before")
        )
    if load.get("prefund_payments"):
        funding = run["payment-funding.json"]
        parents = {txid for _, txid in funding["committed"]}
        assert len(parents) == len(funding["committed"])
        assert funding["outputs"] == summary["planned"] and parents.isdisjoint(written)
        assert all(height <= funding["height"] for height, _ in funding["committed"])
        assert funding["height"] <= min(node["height"] for node in r["before"]["statuses"])
        result["independent_input_funding"] = {
            "funding_transactions": len(parents), "outputs": funding["outputs"],
            "common_height": funding["height"],
        }
    if "lineage-audit.json" in run:
        audit = run["lineage-audit.json"]
        assert audit["applied"] == visible and audit["head_updates"] == 2 * visible
        result["lineage"] = {
            k: audit[k]
            for k in ["applied", "cycles", "fee", "head_updates", "serial_replay_us"]
        }
    return result


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("archives", nargs="+", type=Path)
    print(json.dumps([summarize(p) for p in p.parse_args().archives], indent=2))

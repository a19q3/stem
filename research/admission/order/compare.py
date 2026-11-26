"""Verify matched funded payment workloads and compare four-replica proposal-order trials."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path


def analyze(path):
    with lzma.open(path, "rt") as stream:
        run = json.load(stream)
    assert run["exit"] == 0
    report = run["report.json"]
    summary = report["summary"]
    funding = run["payment-funding.json"]
    assert summary["planned"] == funding["outputs"]
    seen = {txid for b in report["blocks"] for txid in b["ids"]}
    admitted = {d["id"] for d in report["deliveries"] if d["acknowledged_us"] is not None}
    assert len(seen) == sum(len(b["ids"]) for b in report["blocks"]) == summary["visible"]
    assert admitted <= seen
    assert report["finality"] and report["header"] and run["history-index-proof.json"]["header"]
    if report["load"]["complete_volume"]:
        assert admitted == seen == {d["id"] for d in report["deliveries"]}
    resources = zip(report["after"]["resources"], report["before"]["resources"], strict=True)
    cpu_seconds = sum(after["cpu_ticks"] - before["cpu_ticks"] for after, before in resources) / 100
    confirmed_digest = hashlib.sha256()
    for txid in sorted(seen):
        confirmed_digest.update(bytes.fromhex(txid))
    return {
        "file": path.name,
        "binary_sha256": run["binary_sha256"],
        "genesis_sha256": run["genesis.json_sha256"],
        "workload_sha256": run["workload.bin_sha256"],
        "planned": summary["planned"],
        "written": summary["offered"],
        "admitted": summary["admitted"],
        "visible": len(seen),
        "visible_in_window": summary["visible_during_offer"],
        "window_tps": summary["visibility_tps"],
        "drain_tps": summary["finalized_tps_with_drain"],
        "latency_us": summary["latency_us"],
        "cpu_seconds": cpu_seconds,
        "agreed_height": report["agreed_height"],
        "observation_errors": len(report["observation_errors"]),
        "finalized_set_sha256": confirmed_digest.hexdigest(),
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archives", type=Path, nargs="+")
    args = parser.parse_args()
    trials = [analyze(path) for path in args.archives]
    groups = {}
    for trial in trials:
        groups.setdefault(trial["planned"], []).append(trial)
    for group in groups.values():
        assert len({r["workload_sha256"] for r in group}) == 1
        assert len({r["genesis_sha256"] for r in group}) == 1
        complete = [r for r in group if r["visible"] == r["planned"]]
        assert len({r["finalized_set_sha256"] for r in complete}) <= 1
    print(json.dumps(trials, indent=2))

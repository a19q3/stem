"""Compare finality latency with the ordering key of each signed mempool entry."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path


def distribution(values):
    values.sort()
    return {
        f"p{p}_us": values[(len(values) * p + 99) // 100 - 1] if values else None
        for p in (50, 95, 99)
    }


def ordering(archive, workload):
    with lzma.open(archive, "rt") as stream:
        run = json.load(stream)
    assert run["exit"] == 0
    report = run["report.json"]
    included = {txid: b["seen_us"] for b in report["blocks"] for txid in b["ids"]}
    assert len(included) == report["summary"]["visible"]
    bins = [[] for _ in range(16)]
    scheduled, written = [], []
    sha = hashlib.sha256()
    with workload.open("rb") as wire:
        for index, row in enumerate(report["deliveries"]):
            assert row["planned_us"] == index * 1_000_000 // report["load"]["rate"]
            header = wire.read(4)
            assert len(header) == 4
            size = int.from_bytes(header, "little")
            assert 0 < size <= 512 * 1024
            data = wire.read(size)
            assert len(data) == size
            sha.update(header)
            sha.update(data)
            if row["id"] in included:
                seen = included[row["id"]]
                wait = max(0, seen - row["offered_us"])
                order_key = hashlib.blake2b(data, digest_size=32, person=b"ckb-default-hash").digest()
                bins[order_key[0] >> 4].append(wait)
                scheduled.append(max(0, seen - row["planned_us"]))
                written.append(wait)
        assert not wire.read(1)
    assert sha.hexdigest() == run["workload.bin_sha256"]
    assert len(written) == sum(len(rows) for rows in bins) == len(included)
    written_quantiles = distribution(written)
    assert written_quantiles == {k + "_us": v for k, v in report["summary"]["latency_us"].items()}
    return {
        "archive": archive.name,
        "signed_workload_sha256": sha.hexdigest(),
        "scheduled_to_visible": distribution(scheduled),
        "written_to_visible": written_quantiles,
        "latency_by_wire_hash_prefix": {
            format(i, "x"): {"confirmed": len(rows), **distribution(rows)}
            for i, rows in enumerate(bins)
        },
    }


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--workload", type=Path, required=True, help="length-prefixed signed workload from run_load.py --workload-output")
    p.add_argument("archives", type=Path, nargs="+")
    args = p.parse_args()
    print(json.dumps([ordering(a, args.workload) for a in args.archives], indent=2))

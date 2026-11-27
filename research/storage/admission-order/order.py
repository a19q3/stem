"""Match signed payments, local selection keys and certified observation times."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path


def workload(path):
    # storage_load writes uint32 little-endian lengths followed by the signed Strand transactions.
    def digest():
        return hashlib.blake2b(digest_size=32, person=b"ckb-default-hash")

    whole = digest()
    keys = []
    with path.open("rb") as source:
        while size := source.read(4):
            if len(size) != 4:
                raise ValueError("incomplete transaction length")
            n = int.from_bytes(size, "little")
            if not 0 < n <= 512 * 1024:
                raise ValueError("invalid transaction length")
            tx = source.read(n)
            if len(tx) != n:
                raise ValueError("incomplete signed transaction")
            whole.update(size)
            whole.update(tx)
            key = digest()
            key.update(tx)
            keys.append(key.digest())
    return whole.hexdigest(), keys


def percentile(ordered, numerator):
    if not ordered:
        return None
    return ordered[(len(ordered) * numerator + 99) // 100 - 1]


def describe(deliveries, positions, visible, window, end):
    acknowledgments = 0
    window_common = 0
    pending_at_end = 0
    waits = []
    for i in positions:
        sent, ack = deliveries[i]
        seen = visible[i]
        if ack is None:
            continue
        assert sent is not None and ack >= sent
        acknowledgments += 1
        if seen is None or seen > end:
            pending_at_end += 1
        else:
            assert seen >= sent
            waits.append(seen - sent)
            window_common += seen <= window
    waits.sort()
    return {"acknowledged": acknowledgments, "common_in_window": window_common,
            "pending_at_drain": pending_at_end, "median_us": percentile(waits, 50),
            "p99_us": percentile(waits, 99)}


def summarize(path, corpus_hash, keys):
    with lzma.open(path / "payments.json.xz", "rt") as source:
        report = json.load(source)
    bands = bytearray(key[0] * 10 // 256 for key in keys)
    deliveries = report["delivery_times"]
    assert report["workload_id"] == corpus_hash and len(deliveries) == len(bands)
    visible = [None] * len(deliveries)
    multiple = 0
    by_fixed_key = 0
    by_parent_key = 0
    prior_header = bytes.fromhex(report["start_header"])
    network = bytes.fromhex(report["network"])
    for block in report["blocks"]:
        positions = block["positions"]
        parent = hashlib.blake2b(b"Stem Header\0" + network + prior_header,
                                 digest_size=32, person=b"ckb-default-hash").digest()
        prior_header = bytes.fromhex(block["header"])
        assert prior_header[16:48] == parent
        if len(positions) >= 2:
            multiple += 1
            selected = [keys[i] for i in positions]
            by_fixed_key += all(a < b for a, b in zip(selected, selected[1:]))
            by_parent_key += all((a < parent, a) < (b < parent, b)
                                 for a, b in zip(selected, selected[1:]))
        for index in block["positions"]:
            assert 0 <= index < len(bands) and visible[index] is None
            visible[index] = block["seen_us"]
    assert prior_header.hex() == report["header"]
    seconds = report["load"]["seconds"]
    window = seconds * 1_000_000
    end = window + report["load"]["drain"] * 1_000_000
    by_key = [[] for _ in range(10)]
    by_write = [[] for _ in range(4)]
    for i, (written, _) in enumerate(deliveries):
        by_key[bands[i]].append(i)
        if written is not None and written < window:
            by_write[4 * written // window].append(i)

    def rows(groups):
        return [describe(deliveries, g, visible, window, end) for g in groups]

    by_key = rows(by_key)
    assert sum(r["acknowledged"] for r in by_key) == report["summary"]["admitted"]
    stats = rows([range(len(deliveries))])[0]
    sizes = [len(block["positions"]) for block in report["blocks"] if block["positions"]]
    counters = json.loads((path / "counters.json").read_text())
    before = {node["member"]: node["execution"] for node in counters["before"]}
    phase_us = {
        phase: sum(node["execution"][phase]["elapsed_us"] - before[node["member"]][phase]["elapsed_us"]
                   for node in counters["after"])
        for phase in ("proposal", "candidate_verify", "commit")
    }
    return {"workload": corpus_hash, "offer_seconds": seconds, "drain_seconds": report["load"]["drain"],
            "multiple_payment_blocks": multiple, "blocks_in_fixed_order": by_fixed_key,
            "blocks_in_parent_order": by_parent_key, "certified_payments": sum(sizes),
            "payment_blocks": len(sizes), "full_blocks": sizes.count(report["load"]["block_transactions"]),
            "aggregate_execution_us": phase_us,
            "all": stats, "by_first_hash_byte_band": by_key, "by_socket_write_quarter": rows(by_write)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workload", type=Path, required=True)
    parser.add_argument("trials", nargs="+", type=Path)
    args = parser.parse_args()
    corpus_hash, keys = workload(args.workload)
    if len({p.name for p in args.trials}) != len(args.trials):
        parser.error("run each trial series separately so trial names are unique")
    print(json.dumps({p.name: summarize(p, corpus_hash, keys) for p in args.trials}, indent=2))


if __name__ == "__main__":
    main()

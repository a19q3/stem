"""Package a completed four-validator external payment trial and its per-node event traces."""
import argparse
from collections import Counter
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "external"))
from pack import pack


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def snapshot(status):
    admission = status["admission"]
    return {
        "member": status["member"],
        "queue": admission["client_queue"],
        "checks": admission["checks"],
        "verification_us": admission["verification_us"],
        "execution": status["execution"],
    }


def driver_errors(report):
    return dict(Counter(row["error"] for row in report["deliveries"] if row["error"]))


def storage_deltas(report):
    replicas = []
    for before, after in zip(report["before"]["storage"], report["after"]["storage"], strict=True):
        delta = {k: after["rocksdb"][k] - before["rocksdb"][k] for k in ("wal_bytes", "wal_syncs")}
        for purpose in ("journal", "finality"):
            start, end = before["writes"][purpose], after["writes"][purpose]
            for field in ("writes", "sync_microseconds"):
                delta[f"{purpose}_{field}"] = end[field] - start[field]
            if sum(end["sync_buckets"]) - sum(start["sync_buckets"]) != delta[f"{purpose}_writes"]:
                raise ValueError("write count differs from the synchronous write histogram")
        if min(delta.values()) < 0:
            raise ValueError("storage counters regressed")
        replicas.append(delta)
    if len(replicas) != 4:
        raise ValueError("expected storage counters from four validators")
    return replicas


def collect(source, output, executable):
    report, = (source / "measurement").glob("*/report.json")
    with report.open() as f:
        raw = json.load(f)
    output.mkdir()
    metadata = {
        "source_sha256": digest(report), "node_sha256": digest(executable),
        "preparation": json.loads((source / "trial/trial.json").read_bytes()),
        "genesis": json.loads((source / "trial/genesis-id.json").read_bytes()),
        "driver": json.loads((source / "measurement/driver.json").read_bytes()),
        "driver_errors": driver_errors(raw),
        "storage_deltas": storage_deltas(raw),
        "before": [snapshot(s) for s in raw["before"]["statuses"]],
        "after": [snapshot(s) for s in raw["after"]["statuses"]],
        "trace_hashes": {},
    }
    if any([s["member"] for s in metadata[key]] != list(range(4)) for key in ("before", "after")):
        raise ValueError("expected four ordered, stable validator identities")
    trace_files = sorted(source.glob("node-*.trace"))
    if trace_files and [p.name for p in trace_files] != [f"node-{i}.trace" for i in range(4)]:
        raise ValueError("incomplete per-validator event traces")
    for path in trace_files:
        metadata["trace_hashes"][path.name] = digest(path)
        with path.open("rb") as f, lzma.open(output / (path.name + ".xz"), "xb", preset=3) as out:
            shutil.copyfileobj(f, out)
    with lzma.open(output / "payments.json.xz", "xt", preset=4) as out:
        json.dump(pack(raw), out, separators=(",", ":"))
    metadata["payment_sha256"] = digest(output / "payments.json.xz")
    (output / "counters.json").write_text(json.dumps(metadata, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "output", "executable"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    collect(args.source, args.output, args.executable)

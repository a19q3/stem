"""Assign compressed bytes of single-namespace SST data blocks to their namespace."""

import argparse
from collections import Counter
import hashlib
import json
import lzma
from pathlib import Path
import subprocess


def attribute(archive, directory, reader):
    with lzma.open(archive, "rt") as f:
        run = json.load(f)
    files = {}
    total, captured = Counter(), Counter()
    bytes_by_job = {}
    for log, events in run["sst"]["logs"].items():
        node = Path(log).parts[0]
        for event in events:
            name = f"{node}/{event['file_number']:06}.sst"
            assert name not in files
            size, job = event["file_size"], event["kind"]
            total[job] += size
            source = directory / name
            files[name] = {"bytes": size, "job": job}
            if not source.exists():
                continue
            assert source.stat().st_size == size
            result = subprocess.check_output([reader, source], text=True)
            counts = {}
            for line in result.splitlines():
                category, amount, blocks, entries = line.split()
                counts[category] = dict(bytes=int(amount), blocks=int(blocks), entries=int(entries))
            properties = event["table_properties"]
            # Range tombstones are stored in a separate meta block.
            assert sum(r["entries"] for r in counts.values()) + properties["num_range_deletions"] == properties["num_entries"], name
            assert sum(r["blocks"] for r in counts.values()) == properties["num_data_blocks"]
            assert sum(r["bytes"] for r in counts.values()) == size
            assert size - counts["metadata"]["bytes"] == properties["data_size"]
            captured[job] += size
            with source.open("rb") as f:
                files[name]["sha256"] = hashlib.file_digest(f, "sha256").hexdigest()
            files[name]["namespaces"] = counts
            by_category = bytes_by_job.setdefault(job, Counter())
            by_category.update({k: v["bytes"] for k, v in counts.items()})
    assert all(total[job] == run["sst"]["totals"][job]["bytes"] for job in total)
    return {"recorded_bytes": dict(total), "decoded_bytes": dict(captured),
            "encoded_sst_bytes_by_job_and_namespace": bytes_by_job, "files": files}


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("archive", type=Path)
    p.add_argument("captured", type=Path)
    p.add_argument("reader", type=Path)
    args = p.parse_args()
    print(json.dumps(attribute(args.archive, args.captured, args.reader.resolve()), indent=2))

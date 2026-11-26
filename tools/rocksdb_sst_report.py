#!/usr/bin/env python3
"""Count SST file creation in a completed, fresh load run."""

import argparse
import json
from pathlib import Path


JOB_KINDS = {
    "flush_started": "flush",
    "compaction_started": "compaction",
    "recovery_started": "recovery",
}


def sst_events(path):
    events = []
    marker = "EVENT_LOG_v1 "
    with path.open() as log:
        for line in log:
            _, separator, record = line.partition(marker)
            if separator:
                event = json.loads(record)
                if event["event"] in JOB_KINDS or event["event"] == "table_file_creation":
                    events.append(event)
    return events


def report(directory):
    logs = sorted(
        path for pattern in ("LOG", "LOG.old.*")
        for path in directory.glob(f"node-*/db/{pattern}")
    )
    if not logs:
        raise ValueError(f"no replica RocksDB logs under {directory}")
    counts = {kind: {"files": 0, "bytes": 0} for kind in (*JOB_KINDS.values(), "other")}
    reports = {}
    for path in logs:
        events = sst_events(path)
        kinds = {
            event["job"]: JOB_KINDS[event["event"]]
            for event in events if event["event"] in JOB_KINDS
        }
        outputs = []
        for event in events:
            if event["event"] == "table_file_creation" and "file_size" in event:
                kind = kinds.get(event["job"], "other")
                counts[kind]["files"] += 1
                counts[kind]["bytes"] += event["file_size"]
                outputs.append({**event, "kind": kind})
        reports[str(path.relative_to(directory))] = outputs
    return {"totals": counts, "logs": reports}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_directory", type=Path, help="a fresh sustained load fixture output")
    args = parser.parse_args()
    print(json.dumps(report(args.run_directory), separators=(",", ":")))


if __name__ == "__main__":
    main()

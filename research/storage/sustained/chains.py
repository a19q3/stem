"""Trace missing confirmations back to the first gap in each round-robin payment chain."""

import argparse
from collections import Counter
import json
import lzma
from pathlib import Path


def analyze(path):
    with lzma.open(path, "rt") as stream:
        archive = json.load(stream)
    report = archive["report.json"]
    width = report["load"]["payment_chains"]
    assert archive["exit"] == 0 and width > 0
    rows = report["deliveries"]
    included = {txid for block in report["blocks"] for txid in block["ids"]}
    assert len(included) == report["summary"]["visible"]
    first_errors = Counter()
    downstream_errors = Counter()
    first_gaps = []
    for slot in range(width):
        first_gap = None
        for index in range(slot, len(rows), width):
            row = rows[index]
            assert row["planned_us"] == index * 1_000_000 // report["load"]["rate"]
            if row["id"] in included:
                assert first_gap is None, "confirmed a child of an unconfirmed predecessor"
                continue
            reason = row["error"] or "no recorded delivery error"
            if first_gap is None:
                first_gap = index
                first_errors[reason] += 1
                first_gaps.append(row["planned_us"])
            else:
                downstream_errors[reason] += 1
    assert len(included) + first_errors.total() + downstream_errors.total() == len(rows)
    return {
        "archive": path.name,
        "payment_chains": width,
        "chains_completed": width - len(first_gaps),
        "chains_with_gap": len(first_gaps),
        "first_missing_by_delivery_error": first_errors,
        "descendants_of_missing_by_delivery_error": downstream_errors,
        "first_gap_planned_us": min(first_gaps, default=None),
        "last_first_gap_planned_us": max(first_gaps, default=None),
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    print(json.dumps(analyze(args.archive), indent=2))

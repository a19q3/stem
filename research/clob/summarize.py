"""Compute distinct operation and fill rates from a verified native CLOB run."""

import argparse
import json
import math
from pathlib import Path


def percentile(sorted_values, numerator):
    if not sorted_values:
        return None
    index = math.ceil(len(sorted_values) * numerator / 100) - 1
    return sorted_values[index]


def summarize(directory):
    report = json.loads((directory / "report.json").read_bytes())
    audit = json.loads((directory / "lineage-audit.json").read_bytes())
    market = audit["clob"]
    counters = market["operations"]
    seconds = report["load"]["seconds"]
    cutoff = seconds * 1_000_000
    delivered = {
        receipt: block["seen_us"]
        for block in report["blocks"]
        for receipt in block["ids"]
    }
    offered = {
        row["id"]: row["offered_us"]
        for row in report["deliveries"]
        if row["offered_us"] is not None
    }
    matches = market["fills_by_transaction"]
    assert sum(matches.values()) == counters["fills"]
    assert sum(counters[key] for key in ("placements", "cancellations", "rejected")) == audit["included"]
    assert report["summary"]["visible"] == audit["included"]
    assert len(audit["heads"]) == market["markets"]
    assert all(tx in delivered for tx in matches)

    fill_lags = sorted(
        delivered[taker] - offered[maker]
        for maker, taker in market["maker_and_taker_transactions"]
    )
    assert all(lag >= 0 for lag in fill_lags)
    return dict(
        run=str(directory),
        replicas=report["load"]["replicas"],
        markets=market["markets"],
        offered=len(offered),
        confirmed_by_drain=audit["included"],
        confirmation_latency_us=report["summary"]["latency_us"],
        offer_cutoff_backlog=report["summary"]["offered_during_window"] - report["summary"]["visible_during_offer"],
        committed_operations_per_second=report["summary"]["visibility_tps"],
        fills_per_second=sum(count for tx, count in matches.items() if delivered[tx] <= cutoff) / seconds,
        maker_offer_to_common_fill_us={
            f"p{q}": percentile(fill_lags, q) for q in (50, 95, 99)
        },
        timed_fill_samples=len(fill_lags),
        resting_orders=market["resting_orders"],
        verified_height=audit["height"],
        signed_corpus=report["workload_id"],
        **counters,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", nargs="+", type=Path)
    for run in parser.parse_args().evidence:
        print(json.dumps(summarize(run)))

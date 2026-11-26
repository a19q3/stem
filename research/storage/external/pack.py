"""Retain the committed block headers, signed-payment positions and observer clock for replay."""
import argparse
import json
import lzma
from pathlib import Path


def heights(sample):
    return {"statuses": [{"height": s["height"]} for s in sample["statuses"]]}


def pack(report):
    if not report.get("external_endpoints") or not report.get("start_header"):
        raise ValueError("expected a measurement from an external payment driver")
    positions = {row["id"]: i for i, row in enumerate(report["deliveries"])}
    if len(positions) != len(report["deliveries"]):
        raise ValueError("payment trace contains duplicate IDs")
    blocks = [{"seen_us": b["seen_us"], "header": b["header"], "positions": [positions[tx] for tx in b["ids"]]}
              for b in report["blocks"]]
    evidence = {k: report[k] for k in ("network", "workload_id", "start_header", "header", "agreed_height",
                                        "finality", "summary", "load", "external_endpoints", "observation_errors")}
    evidence.update(before=heights(report["before"]), blocks=blocks,
                    delivery_times=[[r["offered_us"], r["acknowledged_us"]] for r in report["deliveries"]],
                    samples=[{"at_us": s["at_us"], **heights(s)} for s in report["samples"]])
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    with args.report.open() as source:
        archive = pack(json.load(source))
    with lzma.open(args.archive, "xt", preset=6) as target:
        json.dump(archive, target, separators=(",", ":"))

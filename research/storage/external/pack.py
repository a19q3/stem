"""Retain the committed block headers, signed-payment positions and observer clock for replay."""
import argparse
import json
import lzma
from pathlib import Path


def heights(sample):
    return {"statuses": [{"height": s["height"]} for s in sample["statuses"]]}


def pack(report, receipt_sample=None):
    if not report.get("external_endpoints") or not report.get("start_header"):
        raise ValueError("expected a measurement from an external payment driver")
    positions = {row["id"]: i for i, row in enumerate(report["deliveries"])}
    if len(positions) != len(report["deliveries"]):
        raise ValueError("payment trace contains duplicate IDs")
    blocks = [{"seen_us": b["seen_us"], "header": b["header"], "positions": [positions[tx] for tx in b["ids"]]}
              for b in report["blocks"]]
    for raw, packed in zip(report["blocks"], blocks):
        if "receipts" in raw:
            packed["receipts"] = raw["receipts"]
    if receipt_sample is not None:
        raw, = [b for b in report["blocks"] if b["height"] == receipt_sample["height"]]
        if len(raw["ids"]) != receipt_sample["transactions"]:
            raise ValueError("receipt sample describes another block")
        packed, = [b for b in blocks if b["header"] == raw["header"]]
        if "receipts" in packed:
            raise ValueError("block already has execution samples")
        for item in receipt_sample["items"]:
            if raw["ids"][item["index"]] != item["tx_id"]:
                raise ValueError("receipt sample describes another payment")
        packed["receipts"] = [item["execution"] for item in receipt_sample["items"]]
    evidence = {k: report[k] for k in ("network", "workload_id", "start_header", "header", "agreed_height",
                                        "finality", "summary", "load", "external_endpoints", "observation_errors")}
    evidence.update(before=heights(report["before"]), blocks=blocks,
                    delivery_times=[[r["offered_us"], r["acknowledged_us"]] for r in report["deliveries"]],
                    samples=[{"at_us": s["at_us"], **heights(s)} for s in report["samples"]])
    if "driver_clock" in report:
        evidence["driver_clock"] = report["driver_clock"]
    if "reused_connections" in report["summary"] and all(
            "connected_us" in row and "reused_connection" in row for row in report["deliveries"]):
        connections = []
        for row in report["deliveries"]:
            connected = row["connected_us"] is not None
            if not connected and (row["offered_us"] is not None or row["reused_connection"]):
                raise ValueError("payment has a socket write or reuse without a connection")
            connections.append(row["reused_connection"] if connected else None)
        evidence["submission_connections"] = connections
    if report["load"].get("ready_senders", False):
        evidence["delivery_routes"] = [row.get("endpoint") for row in report["deliveries"]]
    if any("queue_full_us" in row for row in report["deliveries"]):
        evidence["queue_full_us"] = [row.get("queue_full_us") for row in report["deliveries"]]
    replies = [[i, row["response_us"]] for i, row in enumerate(report["deliveries"])
               if row["acknowledged_us"] is None and row["response_us"] is not None]
    if replies:
        evidence["unacknowledged_responses"] = replies
    if report["load"].get("pause_on_full"):
        pauses = []
        for i, row in enumerate(report["deliveries"]):
            interval = row.get("full_pause_us")
            if (interval is not None) != (row["error"] == "mempool capacity exceeded"):
                raise ValueError("pause trace differs from full-mempool responses")
            if interval is not None:
                begin, end = interval
                if row["response_us"] is None or row["acknowledged_us"] is not None or not row["response_us"] <= begin <= end:
                    raise ValueError("invalid pause after full-mempool response")
                pauses.append([i, begin, end])
        evidence["full_mempool_pauses"] = pauses
    return evidence


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--receipts", type=Path, help="import a separately captured sample from this archive node")
    args = parser.parse_args()
    with args.report.open() as source:
        archive = pack(json.load(source), json.loads(args.receipts.read_bytes()) if args.receipts else None)
    with lzma.open(args.archive, "xt", preset=6) as target:
        json.dump(archive, target, separators=(",", ":"))

"""Replay a prepared payment trial and score consecutive one-minute acceptance windows."""
import argparse
import hashlib
import ipaddress
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

MINUTE_US = 60_000_000
MINIMUM_WINDOWS = 3


def parse_endpoint(endpoint):
    hostname, port = endpoint.rsplit(":", 1)
    return ipaddress.ip_address(hostname.strip("[]")), int(port)


def host_inventory(inventory, endpoints, peers):
    measured = [parse_endpoint(endpoint) for endpoint in endpoints]
    prepared = [parse_endpoint(peer["address"]) for peer in peers]
    if len(set(measured)) != 4 or measured != prepared:
        raise ValueError("measured validator endpoints must match the four prepared peers in order")
    validators = inventory["validators"]
    if [parse_endpoint(node["endpoint"]) for node in validators] != measured:
        raise ValueError("inventory validators must match the four measured endpoints in order")
    hosts = [node["physical_host"] for node in validators] + [inventory["driver"]["physical_host"]]
    if any(not isinstance(host, str) or not host.strip() for host in hosts) or len({host.strip().lower() for host in hosts}) != 5:
        raise ValueError("record four distinct physical validators and a separate driver host")
    for address, port in measured:
        address = getattr(address, "ipv4_mapped", None) or address
        if address.is_loopback or address.is_unspecified or not 0 < port < 65536:
            raise ValueError("use a remote address for every validator")


def counts_in_windows(observations, warmup_seconds, windows):
    """Count events in intervals open on the left and closed on the right."""
    start = warmup_seconds * 1_000_000
    counts = [0] * windows
    for observed in observations:
        if observed is not None and 0 < (offset := observed - start) <= windows * MINUTE_US:
            counts[(offset - 1) // MINUTE_US] += 1
    return counts


def score(report, settings, *, mode, target, warmup_seconds, max_unacknowledged_bps,
          max_pending, max_pending_growth=None, max_p99_ms=None):
    load = report["load"]
    seconds = load["seconds"]
    rate = settings["RATE"]
    measured_seconds = seconds - warmup_seconds
    minute_count = max(0, measured_seconds // 60)
    window_start = warmup_seconds * 1_000_000
    window_end = seconds * 1_000_000
    deadline = window_end + load["drain"] * 1_000_000
    summary = report["summary"]
    deliveries = report["delivery_times"]
    visibility = [None] * len(deliveries)
    for block in report["blocks"]:
        for index in block["positions"]:
            visibility[index] = block["seen_us"]

    writes = counts_in_windows((write for write, _ in deliveries), warmup_seconds, minute_count)
    acknowledged = counts_in_windows((ack for _, ack in deliveries), warmup_seconds, minute_count)
    common = counts_in_windows(visibility, warmup_seconds, minute_count)
    pending_at_deadline = 0
    written_without_ack = 0
    waits = []
    pending_counts = [0] * (minute_count + 1)
    # An acknowledgment is pending from its response until the first common observation.
    # Process visibility before acknowledgments with the same timestamp.
    pending_events = []
    for (write, ack), seen in zip(deliveries, visibility, strict=True):
        during_measurement = write is not None and window_start < write <= window_end
        if ack is None:
            written_without_ack += during_measurement
            continue
        if seen is None or seen > deadline:
            pending_at_deadline += 1
        elif during_measurement:
            waits.append(seen - write)
        if seen is not None and seen <= ack:
            continue
        if window_start < ack <= window_end:
            pending_events.append(2 * ack + 1)
        if seen is not None and window_start < seen <= window_end:
            pending_events.append(2 * seen)
        first_pending = max(0, (ack - window_start + MINUTE_US - 1) // MINUTE_US)
        last_pending = minute_count if seen is None else min(minute_count, (seen - window_start - 1) // MINUTE_US)
        for boundary in range(first_pending, last_pending + 1):
            pending_counts[boundary] += 1

    pending = peak_pending = pending_counts[0]
    for event in sorted(pending_events):
        pending += 1 if event % 2 else -1
        peak_pending = max(peak_pending, pending)
    lowest_pending = pending_counts[0]
    pending_growth = 0
    for count in pending_counts[1:]:
        pending_growth = max(pending_growth, count - lowest_pending)
        lowest_pending = min(lowest_pending, count)

    waits.sort()
    p99 = waits[(len(waits) * 99 + 99) // 100 - 1] if waits else None
    reasons = []
    if (settings["REPLICAS"], settings["PAYMENT_DATA_BYTES"], settings["PREFUND_PAYMENTS"]) != (4, 0, 1) or load["payment_chains"]:
        reasons.append("profile requires four validators processing independent ordinary prefunded payments")
    if warmup_seconds < 60 or measured_seconds % 60 or minute_count < MINIMUM_WINDOWS or load["drain"] <= 0:
        reasons.append("profile needs at least 60 seconds of warmup, three complete measurement minutes and a positive drain")
    if mode == "sustainable" and rate != target:
        reasons.append("sustainable offer rate must equal the target")
    if mode == "overload" and rate <= target:
        reasons.append("overload offer rate must exceed the target")
    written = sum(writes)
    if written * 1_000 < max(0, measured_seconds) * rate * 999 or any(count < 59 * rate for count in writes):
        reasons.append("actual writes did not sustain the prepared offer")
    if sum(common) < target * max(0, measured_seconds) or any(count < 59 * target for count in common):
        reasons.append("common confirmations did not sustain the target")
    if written_without_ack * 10_000 > written * max_unacknowledged_bps:
        reasons.append("measurement write-without-ack rate exceeded its limit")
    if peak_pending > max_pending:
        reasons.append("peak acknowledged backlog exceeded its limit")
    if max_pending_growth is not None and pending_growth > max_pending_growth:
        reasons.append("acknowledged backlog grew beyond its limit across minute boundaries")
    if max_p99_ms is not None and (p99 is None or p99 > 1000 * max_p99_ms):
        reasons.append("measurement p99 exceeded its limit or was unavailable")
    if pending_at_deadline:
        reasons.append("acknowledged payments were still pending at the drain deadline")
    return {
        "mode": mode, "scheduled_tps": rate, "target_tps": target, "warmup_seconds": warmup_seconds,
        "measured_seconds": measured_seconds,
        "limits": {"unacknowledged_bps": max_unacknowledged_bps, "pending": max_pending,
                   "pending_growth": max_pending_growth, "p99_ms": max_p99_ms},
        "writes_per_minute": writes, "acknowledgments_per_minute": acknowledged, "common_per_minute": common,
        "measurement_writes_without_ack": written_without_ack,
        "acknowledged_pending_at_measurement_start_and_minute_ends": pending_counts,
        "acknowledged_pending_peak": peak_pending, "acknowledged_pending_growth": pending_growth,
        "acknowledged_pending_at_drain": pending_at_deadline,
        "write_to_common_p99_us": p99, "write_to_common_samples": len(waits),
        "planned": summary["planned"], "written_total": summary["offered"], "acknowledged_total": summary["admitted"],
        "client_drops": summary["client_drops"], "transport_write_failures": summary["transport_write_failures"],
        "writes_without_successful_ack": summary["offered"] - summary["admitted"],
        "common_without_successful_ack": summary["visible_without_ack"],
        "observer_error_count": len(report["observation_errors"]),
        "workload_id": report["workload_id"], "agreed_height": report["agreed_height"],
        "passed": not reasons, "reasons": reasons,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("harness", "contracts", "trial", "report", "inventory"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--mode", choices=("sustainable", "overload"), required=True)
    parser.add_argument("--max-unacknowledged-bps", type=int)
    parser.add_argument("--max-pending", type=int)
    parser.add_argument("--max-pending-growth", type=int, help="sustainable mode only")
    parser.add_argument("--max-p99-ms", type=int, help="sustainable mode only")
    parser.add_argument("--warmup-seconds", type=int, default=60)
    parser.add_argument("--target-tps", type=int, default=3000)
    args = parser.parse_args()
    if args.warmup_seconds < 60 or args.target_tps <= 0:
        parser.error("warmup must be at least 60 seconds and target must be positive")
    if args.mode == "overload":
        if args.max_pending is None or args.max_unacknowledged_bps is None:
            parser.error("overload requires --max-pending and --max-unacknowledged-bps")
        if args.max_pending_growth is not None or args.max_p99_ms is not None:
            parser.error("growth and p99 limits apply to sustainable mode")
    else:
        if args.max_unacknowledged_bps is None:
            args.max_unacknowledged_bps = 10
        if args.max_pending is None:
            args.max_pending = 5 * args.target_tps
        if args.max_pending_growth is None:
            args.max_pending_growth = args.target_tps
        if args.max_p99_ms is None:
            args.max_p99_ms = 5000
    if not 0 <= args.max_unacknowledged_bps < 10_000 or any(
        value is not None and value < 0
        for value in (args.max_pending, args.max_pending_growth, args.max_p99_ms)
    ):
        parser.error("limits must be nonnegative and unacknowledged basis points must be below 10000")
    inventory_bytes = args.inventory.read_bytes()
    inventory = json.loads(inventory_bytes)
    here = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory() as tmp:
        scratch = Path(tmp)
        snapshot = scratch / "report.json.xz"
        trial = scratch / "trial"
        trial.mkdir()
        for filename in ("trial.json", "replicas.json", "genesis.json"):
            shutil.copyfile(args.trial / filename, trial / filename)
        shutil.copyfile(args.report, snapshot)
        plan = json.loads((trial / "trial.json").read_bytes())
        settings = plan["storage_settings"]
        if "inventory_sha256" in plan and plan["inventory_sha256"] != hashlib.sha256(inventory_bytes).hexdigest():
            parser.error("physical inventory differs from the prepared V1 plan")
        peers = json.loads((trial / "replicas.json").read_bytes())["peers"]
        subprocess.run([sys.executable, here / "drive.py", "--harness", args.harness, "--contracts", args.contracts,
                        "verify", "--trial", trial, "--report", snapshot], stdout=sys.stderr, check=True)
        with lzma.open(snapshot, "rt") as source:
            report = json.load(source)
        with snapshot.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = score(
        report, settings, mode=args.mode, target=args.target_tps, warmup_seconds=args.warmup_seconds,
        max_unacknowledged_bps=args.max_unacknowledged_bps, max_pending=args.max_pending,
        max_pending_growth=args.max_pending_growth, max_p99_ms=args.max_p99_ms,
    )
    try:
        host_inventory(inventory, report["external_endpoints"], peers)
    except (TypeError, ValueError, KeyError) as error:
        result["reasons"].append(f"inventory: {error}")
        result["passed"] = False
    result["report_sha256"] = digest
    if "qualification_profile" in plan:
        result["qualification_profile"] = plan["qualification_profile"]
    result["physical_hosts_recorded_by_operator"] = inventory
    print(json.dumps(result, indent=2))
    sys.exit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()

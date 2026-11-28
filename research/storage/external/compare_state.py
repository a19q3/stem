"""Independently replay two Cell engine measurements of one prepared workload."""

import argparse
import json
import lzma
from pathlib import Path
import subprocess
import sys
import tempfile

if __package__:
    from .drive import digest
    from .qualify import parse_endpoint
    from .runtime import read_runtime
else:
    from drive import digest
    from qualify import parse_endpoint
    from runtime import read_runtime


ENGINES = {"patricia": "compact-patricia", "jmt": "sha256-jmt-0.12"}
ROCKSDB_COUNTERS = ("wal_bytes", "wal_syncs", "flush_bytes", "compaction_bytes")


def observation(path):
    """Extract measurement metadata and counters without retaining the full trace."""
    open_report = lzma.open if path.suffix == ".xz" else open
    with open_report(path, "rt") as source:
        report = json.load(source)
    storage = None
    before = report["before"].get("storage")
    after = report.get("after", {}).get("storage")
    if before or after:
        if not before or not after or len(before) != len(after) or len(before) != report["load"]["replicas"]:
            raise ValueError(f"{path}: incomplete RocksDB counters")
        storage = []
        for start, end in zip(before, after, strict=True):
            delta = {field: end["rocksdb"][field] - start["rocksdb"][field] for field in ROCKSDB_COUNTERS}
            if min(delta.values()) < 0:
                raise ValueError(f"{path}: RocksDB counters decreased during measurement")
            storage.append(delta)
    settings = ("history_window", "live_cell_index", "cell_node_index", "verification_cache")
    node_status = {
        label: [{name: status[name] for name in settings if name in status}
                for status in report.get(label, {}).get("statuses", ())]
        for label in ("before", "after")
    }
    return {
        "load": report["load"],
        "reported_node_resources": node_status,
        "limits": report["limits"],
        "external_endpoints": [parse_endpoint(address) for address in report["external_endpoints"]],
        "rocksdb_counters_by_replica": storage,
    }


def check_reported_resources(runtime, observation):
    """Check published resource settings and retain observed index residency."""
    cache_profiles = []
    mib = 1 << 20
    for when, statuses in observation["reported_node_resources"].items():
        profiles = []
        for node, status in zip(runtime["replicas"], statuses, strict=True):
            config = node["configuration"]
            if config["storage_metrics"] != observation["load"]["measured"]:
                raise ValueError(f"{when}: storage metric settings differ for {node['endpoint']}")
            if status["history_window"] != config["history_window"]:
                raise ValueError(f"{when}: reported retention differs for {node['endpoint']}")
            for field, allowance in (
                ("live_cell_index", config["live_cell_index_mib"]),
                ("cell_node_index", config["cell_node_index_mib"]),
            ):
                reported = status[field]
                limit = None if reported is None else reported["limit_bytes"]
                if limit != (allowance * mib if allowance is not None else None):
                    raise ValueError(f"{when}: reported {field} budget differs for {node['endpoint']}")
            live = status["live_cell_index"]
            if live is not None:
                layout = "interned_scripts" if config["live_cell_index_intern_scripts"] else "inline"
                if live["layout"] != layout or live["recovery_workers"] != config["live_cell_recovery_workers"]:
                    raise ValueError(f"{when}: reported Live Cell configuration differs for {node['endpoint']}")
            cache = status["verification_cache"]
            profiles.append({
                "capacity": cache["capacity"],
                "shards": cache["shards"],
                "lock_sample_interval": cache["lock_sample_interval"],
            })
        cache_profiles.append(profiles)
    if cache_profiles[0] != cache_profiles[1]:
        raise ValueError("reported verification cache configuration changed during measurement")
    return cache_profiles[0]


def replay(args, engine, report, scratch):
    receipt = scratch / f"{engine}.json"
    subprocess.run([
        sys.executable, Path(__file__).with_name("drive.py"),
        "--harness", args.harness, "--contracts", args.contracts, "--engine", engine,
        "verify", "--trial", args.trial, "--report", report, "--verified-output", receipt,
    ], check=True)
    return json.loads(receipt.read_bytes())


def audit(args, engine, home, verified):
    command = [
        args.binary.resolve(), "research-cell-audit", "--home", home,
        "--genesis", args.trial / "genesis.json",
        "--expected-header", verified["header"], "--measured-height", str(verified["height"]),
        "--expected-genesis", verified["genesis"], "--proof-batch-width", str(args.proof_batch_width),
    ]
    if engine == "jmt":
        command += ["--jmt-proof-cache-nodes", str(args.jmt_proof_cache_nodes)]
    elif args.patricia_proof_frontier:
        command.append("--patricia-proof-frontier")
    if args.shared_proof_record_batches:
        command.append("--shared-proof-record-batches")
    result = json.loads(subprocess.check_output(command))
    if result["engine"] != ENGINES[engine]:
        raise ValueError(f"{home}: recovered database uses {result['engine']}, expected {ENGINES[engine]}")
    expected_frontier = args.patricia_proof_frontier and engine == "patricia"
    if (result["proof_batch_width"] != args.proof_batch_width
            or result.get("shared_proof_record_batches", False) != args.shared_proof_record_batches
            or result.get("patricia_proof_frontier", False) != expected_frontier):
        raise ValueError(f"{home}: proof audit did not use the requested read paths")
    if args.shared_proof_record_batches:
        expected_keys = result["membership_queries"] + result["absence_queries"]
        if result["proof_reads"]["cells"]["keys"] != expected_keys:
            raise ValueError(f"{home}: proof audit did not read every sampled record")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("harness", "contracts", "trial", "patricia-report", "jmt-report", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    for name in ("binary", "patricia-home", "jmt-home"):
        parser.add_argument(f"--{name}", type=Path)
    parser.add_argument("--patricia-runtime", type=Path)
    parser.add_argument("--jmt-runtime", type=Path)
    parser.add_argument("--proof-batch-width", type=int, default=32)
    parser.add_argument("--jmt-proof-cache-nodes", type=int, default=0)
    parser.add_argument("--shared-proof-record-batches", action="store_true")
    parser.add_argument("--patricia-proof-frontier", action="store_true")
    args = parser.parse_args()
    homes = (args.patricia_home, args.jmt_home)
    if any(homes) and (not all(homes) or args.binary is None):
        parser.error("recovery audit needs the research binary and both stopped replica homes")
    audit_recovery = all(homes)
    if audit_recovery and homes[0].samefile(homes[1]):
        parser.error("the two replicas must have separate stores")
    if args.output.exists():
        parser.error("the output already exists")

    plan_path = args.trial / "trial.json"
    plan = json.loads(plan_path.read_bytes())
    if digest(args.harness) != plan["harness_sha256"]:
        parser.error("the paired comparison requires the same harness as trial preparation")
    reports = (args.patricia_report, args.jmt_report)
    if reports[0].samefile(reports[1]):
        parser.error("the two engine runs must have separate reports")
    observations = [observation(path) for path in reports]
    peers = json.loads((args.trial / "replicas.json").read_bytes())["peers"]
    endpoints = [parse_endpoint(peer["address"]) for peer in peers]
    for field in ("load", "limits", "external_endpoints"):
        if observations[0][field] != observations[1][field]:
            parser.error(f"engine arms disagree on {field}")
    if observations[0]["external_endpoints"] != endpoints:
        parser.error("measurement endpoints differ from the prepared committee")
    for name, value in plan["storage_settings"].items():
        field = "cadence_ms" if name == "CADENCE" else name.lower()
        if observations[0]["load"].get(field) != value:
            parser.error(f"measurement differs from the prepared {field} setting")
    genesis_limits = json.loads((args.trial / "genesis.json").read_bytes())["limits"]
    if observations[0]["limits"] != genesis_limits:
        parser.error("measurement limits differ from the prepared genesis")
    left, right = (row["rocksdb_counters_by_replica"] for row in observations)
    if (left is None) != (right is None):
        parser.error("RocksDB counter coverage differs across arms")

    runtime_paths = (args.patricia_runtime, args.jmt_runtime)
    runtimes = (None, None)
    if any(runtime_paths):
        if not all(runtime_paths):
            parser.error("supply runtime evidence for both engine arms")
        try:
            runtimes = tuple(read_runtime(path, report, endpoints)
                             for path, report in zip(runtime_paths, reports, strict=True))
        except (OSError, ValueError, KeyError, TypeError) as error:
            parser.error(f"validator runtime: {error}")
        for field in ("cpu_ticks_per_second", "driver_cpu_affinity"):
            if runtimes[0][field] != runtimes[1][field]:
                parser.error(f"engine arms disagree on driver {field}")
        for left, right in zip(*(runtime["replicas"] for runtime in runtimes), strict=True):
            if left["configuration"] != right["configuration"]:
                parser.error(f"engine arms disagree on validator configuration for {left['endpoint']}")
        try:
            verification = [check_reported_resources(runtime, observed)
                            for runtime, observed in zip(runtimes, observations, strict=True)]
        except (ValueError, KeyError, TypeError) as error:
            parser.error(f"reported resources: {error}")
        if verification[0] != verification[1]:
            parser.error("engine arms disagree on reported verification cache configuration")

    with tempfile.TemporaryDirectory() as tmp:
        verified = [replay(args, engine, path, Path(tmp)) for engine, path in zip(ENGINES, reports, strict=True)]
    for field in ("network", "workload_id"):
        if verified[0][field] != verified[1][field]:
            parser.error(f"engine arms disagree on independently verified {field}")
    if verified[0]["genesis"] == verified[1]["genesis"]:
        parser.error("expected separately authenticated Patricia and JMT genesis commitments")

    result = {
        "trial_sha256": digest(plan_path),
        "genesis_spec_sha256": plan["genesis_sha256"],
        "harness_sha256": plan["harness_sha256"],
        "matched_load": observations[0]["load"],
        "matched_limits": observations[0]["limits"],
        "matched_runtime_resources": True if all(runtime_paths) else None,
        "same_confirmed_set_at_drain": verified[0]["confirmed_set_blake2b"] == verified[1]["confirmed_set_blake2b"],
        "engines": {
            engine: {"replay": replayed, "report_sha256": digest(path),
                     "rocksdb_counters_by_replica": observed["rocksdb_counters_by_replica"],
                     "runtime": runtime, "reported_node_resources": observed["reported_node_resources"]}
            for engine, replayed, path, observed, runtime in zip(
                ENGINES, verified, reports, observations, runtimes, strict=True
            )
        },
    }
    if audit_recovery:
        result["audit_binary_sha256"] = digest(args.binary)
        for engine, home, replayed in zip(ENGINES, homes, verified, strict=True):
            result["engines"][engine]["recovery_audit"] = audit(args, engine, home, replayed)
        left, right = [entry["recovery_audit"] for entry in result["engines"].values()]
        digests = (left.get("proof_query_digest"), right.get("proof_query_digest"))
        same_queries = digests[0] == digests[1] if all(digests) else None
        result["same_recovered_proof_queries"] = same_queries
        if same_queries and args.shared_proof_record_batches:
            for field in ("calls", "keys", "multi_get_calls"):
                if left["proof_reads"]["cells"][field] != right["proof_reads"]["cells"][field]:
                    parser.error(f"matched proof queries disagree on record read {field}")
    with args.output.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()

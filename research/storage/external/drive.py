"""Prepare or measure a prefunded payment trial with independently started Jolteon nodes."""

import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, required=True, help="compiled native_store integration test")
    parser.add_argument("--contracts", type=Path, required=True, help="directory containing stem-lock")
    phases = parser.add_subparsers(dest="phase", required=True)
    prepare = phases.add_parser("prepare")
    prepare.add_argument("--topology", type=Path, required=True, help="Jolteon configuration with public peer addresses")
    prepare.add_argument("--rate", type=int, required=True)
    prepare.add_argument("--seconds", type=int, required=True)
    prepare.add_argument("--senders", type=int, default=64)
    prepare.add_argument("--shard-senders", action="store_true", help="reserve an equal sender pool and queue per endpoint")
    prepare.add_argument("--drain", type=int, default=15)
    prepare.add_argument("--data-bytes", type=int, default=0)
    prepare.add_argument("--block-transactions", type=int, default=1024)
    prepare.add_argument("--block-cycles", type=int, default=2_000_000_000)
    prepare.add_argument("--complete-volume", action="store_true")
    prepare.add_argument("--reuse-submissions", action="store_true")
    measure = phases.add_parser("measure")
    measure.add_argument("--output", type=Path, required=True, help="new directory for this measurement")
    verify = phases.add_parser("verify")
    verify.add_argument("--report", type=Path, required=True)
    for phase in (prepare, measure, verify):
        phase.add_argument("--trial", type=Path, required=True, help="genesis and measurement plan directory")
    args = parser.parse_args()
    trial = args.trial.resolve()
    preparing = args.phase == "prepare"
    harness_hash = digest(args.harness)
    lock_hash = digest(args.contracts / "stem-lock")
    if preparing:
        topology = args.topology.resolve()
        config = json.loads(topology.read_bytes())
        settings = {"RATE": args.rate, "SECONDS": args.seconds, "DRAIN": args.drain, "SENDERS": args.senders,
                    "CADENCE": config["proposal_interval_ms"], "VM_WORKERS": config.get("vm_workers", 2),
                    "PAYMENT_DATA_BYTES": args.data_bytes, "COMPLETE_VOLUME": int(args.complete_volume),
                    "REPLICAS": len(config["peers"]), "PREFUND_PAYMENTS": 1,
                    "REUSE_SUBMISSIONS": int(args.reuse_submissions),
                    "SHARD_SENDERS": int(args.shard_senders), "BLOCK_TRANSACTIONS": args.block_transactions, "BLOCK_CYCLES": args.block_cycles}
        plan = {"storage_settings": settings, "harness_sha256": harness_hash, "lock_sha256": lock_hash,
                "topology_sha256": digest(topology)}
    else:
        plan = json.loads((trial / "trial.json").read_bytes())
        topology = trial / "replicas.json"
        if (plan["lock_sha256"], plan["topology_sha256"], plan["genesis_sha256"]) != (
                lock_hash, digest(topology), digest(trial / "genesis.json")):
            parser.error("contract, topology or genesis differs from the prepared trial")
        settings = plan["storage_settings"]
        if args.phase == "measure":
            args.output.mkdir(mode=0o700)
            (args.output / "driver.json").write_text(json.dumps({"harness_sha256": harness_hash}) + "\n")
    with tempfile.TemporaryDirectory() as scratch:
        env = {k: v for k, v in os.environ.items() if not k.startswith("STEM_STORAGE_")}
        env.update({"STEM_STORAGE_" + k: str(v) for k, v in settings.items()})
        env.update(STEM_STORAGE_TOPOLOGY=str(topology), STEM_TEST_CONTRACT_DIR=str(args.contracts.resolve()),
                   STEM_TEST_EVIDENCE_ROOT=str(args.output.resolve()) if args.phase == "measure" else scratch)
        if args.phase == "verify":
            report = args.report.resolve()
            if report.suffix == ".xz":
                expanded = Path(scratch) / "report.json"
                with lzma.open(report) as source, expanded.open("wb") as destination:
                    shutil.copyfileobj(source, destination)
                report = expanded
            env["STEM_STORAGE_VERIFY_REPORT"] = str(report)
        if preparing:
            env["STEM_STORAGE_GENESIS_OUTPUT"] = str(trial)
        subprocess.run([args.harness.resolve(), "--exact", "storage_load::sustained_payment_storage",
                        "--ignored", "--nocapture"], env=env, check=True)
    if preparing:
        shutil.copyfile(topology, trial / "replicas.json")
        plan["genesis_sha256"] = digest(trial / "genesis.json")
        (trial / "trial.json").write_text(json.dumps(plan, indent=2) + "\n")
    elif args.phase == "measure":
        report, = args.output.glob("stem-storage-*/report.json")
        print(report)


if __name__ == "__main__":
    main()

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

if __package__:
    from .qualify import host_inventory
else:
    from qualify import host_inventory


V1_RATES = {"baseline": 1800, "sustainable": 3000, "overload": 3300}


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
    routing = prepare.add_mutually_exclusive_group()
    routing.add_argument("--shard-senders", action="store_true", help="reserve an equal sender pool and queue per endpoint")
    routing.add_argument("--ready-senders", action="store_true", help="reserve equal senders per endpoint pulling from one shared queue")
    prepare.add_argument("--drain", type=int, default=15)
    prepare.add_argument("--data-bytes", type=int, default=0)
    prepare.add_argument("--funded-cells", type=int,
                         help="pre-fund this many payment Cells independently of the offered count")
    prepare.add_argument("--payment-type-groups", type=int, choices=range(13), default=0,
                         help="cycle through zero to this many Type Groups on independent payments")
    prepare.add_argument("--block-transactions", type=int, default=1024)
    prepare.add_argument("--block-cycles", type=int, default=2_000_000_000)
    prepare.add_argument("--complete-volume", action="store_true")
    prepare.add_argument("--reuse-submissions", action="store_true")
    prepare.add_argument("--pause-on-full", action="store_true",
                         help="pause a sender for one proposal interval after a full-mempool reply")
    v1 = phases.add_parser("prepare-v1", help="prepare a fixed V1 cross-host profile")
    v1.add_argument("--topology", type=Path, required=True)
    v1.add_argument("--inventory", type=Path, help="verify and pin the operator's physical placement record")
    v1.add_argument("--profile", choices=V1_RATES, required=True)
    v1.set_defaults(seconds=240, drain=45, senders=256, data_bytes=0,
                    block_transactions=2048, block_cycles=4_000_000_000, complete_volume=True,
                    reuse_submissions=True, pause_on_full=False, shard_senders=False, ready_senders=True)
    measure = phases.add_parser("measure")
    measure.add_argument("--output", type=Path, required=True, help="new directory for this measurement")
    funding = measure.add_mutually_exclusive_group()
    funding.add_argument("--fund-only", action="store_true", help="prepare funded replicas and exit before offering load")
    funding.add_argument("--reuse-funding", type=Path, help="require the funded Header recorded by a previous run")
    verify = phases.add_parser("verify")
    verify.add_argument("--report", type=Path, required=True)
    for phase in (prepare, v1, measure, verify):
        phase.add_argument("--trial", type=Path, required=True, help="genesis and measurement plan directory")
    args = parser.parse_args()
    trial = args.trial.resolve()
    v1_profile = args.phase == "prepare-v1"
    preparing = args.phase == "prepare" or v1_profile
    harness_hash = digest(args.harness)
    lock_hash = digest(args.contracts / "stem-lock")
    if preparing:
        topology = args.topology.resolve()
        config = json.loads(topology.read_bytes())
        if v1_profile:
            args.rate = V1_RATES[args.profile]
            peers = config["peers"]
            if (len(peers), config.get("proposal_interval_ms"), config.get("vm_workers", 2)) != (4, 100, 2):
                parser.error("V1 qualification requires four validators, a 100 ms proposal interval and two VM workers")
            if args.inventory is not None:
                try:
                    inventory = args.inventory.read_bytes()
                    host_inventory(json.loads(inventory), [peer["address"] for peer in peers], peers)
                except (OSError, KeyError, TypeError, ValueError) as error:
                    parser.error(f"V1 inventory: {error}")
        settings = {"RATE": args.rate, "SECONDS": args.seconds, "DRAIN": args.drain, "SENDERS": args.senders,
                    "CADENCE": config["proposal_interval_ms"], "VM_WORKERS": config.get("vm_workers", 2),
                    "PAYMENT_DATA_BYTES": args.data_bytes, "COMPLETE_VOLUME": int(args.complete_volume),
                    "REPLICAS": len(config["peers"]), "PREFUND_PAYMENTS": 1,
                    "REUSE_SUBMISSIONS": int(args.reuse_submissions), "PAUSE_ON_FULL": int(args.pause_on_full),
                    "SHARD_SENDERS": int(args.shard_senders), "READY_SENDERS": int(args.ready_senders),
                    "BLOCK_TRANSACTIONS": args.block_transactions, "BLOCK_CYCLES": args.block_cycles}
        if not v1_profile and args.payment_type_groups:
            if args.data_bytes:
                parser.error("mixed Type Groups use the fixture's canonical application data")
            settings.update(PREFUND_PAYMENTS=0, PAYMENT_TYPE_GROUPS=args.payment_type_groups)
        if not v1_profile and args.funded_cells is not None:
            if args.payment_type_groups or not args.rate * args.seconds <= args.funded_cells <= 2_000_000:
                parser.error("funded Cells require ordinary payments and a count between offers and 2,000,000")
            settings["FUNDED_CELLS"] = args.funded_cells
        plan = {"storage_settings": settings, "harness_sha256": harness_hash, "lock_sha256": lock_hash,
                "topology_sha256": digest(topology)}
        if v1_profile:
            plan["qualification_profile"] = args.profile
            if args.inventory is not None:
                plan["inventory_sha256"] = hashlib.sha256(inventory).hexdigest()
    else:
        plan = json.loads((trial / "trial.json").read_bytes())
        topology = trial / "replicas.json"
        if (plan["lock_sha256"], plan["topology_sha256"], plan["genesis_sha256"]) != (
                lock_hash, digest(topology), digest(trial / "genesis.json")):
            parser.error("contract, topology or genesis differs from the prepared trial")
        settings = plan["storage_settings"]
        if args.phase == "measure":
            if (args.fund_only or args.reuse_funding) and not settings["PREFUND_PAYMENTS"]:
                parser.error("funding options require the pre-funded payment workload")
            args.output.mkdir(mode=0o700)
            (args.output / "driver.json").write_text(json.dumps({"harness_sha256": harness_hash}) + "\n")
    with tempfile.TemporaryDirectory() as scratch:
        env = {k: v for k, v in os.environ.items() if not k.startswith("STEM_STORAGE_")}
        env.update({"STEM_STORAGE_" + k: str(v) for k, v in settings.items()})
        env.update(STEM_STORAGE_TOPOLOGY=str(topology), STEM_TEST_CONTRACT_DIR=str(args.contracts.resolve()),
                   STEM_TEST_EVIDENCE_ROOT=str(args.output.resolve()) if args.phase == "measure" else scratch)
        if args.phase == "measure" and args.fund_only:
            env["STEM_TEST_FUND_ONLY"] = "1"
        if args.phase == "measure" and args.reuse_funding:
            env["STEM_TEST_REUSE_FUNDING"] = str(args.reuse_funding.resolve())
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
        name = "payment-funding.json" if args.fund_only else "report.json"
        report, = args.output.glob("stem-storage-*/" + name)
        print(report)


if __name__ == "__main__":
    main()

"""Archive the native replicated-load evidence before releasing disposable database files."""

import argparse
import hashlib
import json
import lzma
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

from summarize import device_writes

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    for name in ["harness", "binary", "contracts", "time", "output", "scratch"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--complete-volume", action="store_true")
    parser.add_argument("--device-accounting", action="store_true",
                        help="run each validator in its own Linux systemd user I/O scope on a block-backed filesystem")
    parser.add_argument("--rate", type=int, required=True)
    parser.add_argument("--seconds", type=int, required=True)
    parser.add_argument("--payment-chains", type=int, default=0, help="number of independently recycled payment outputs")
    parser.add_argument("--prefund-payments", action="store_true",
                        help="finalize disjoint payment inputs on all nodes before measurement")
    parser.add_argument("--workload-output", type=Path,
                        help="also save the exact length-prefixed signed transactions for independent replay")
    parser.add_argument("--data-bytes", type=int, default=0)
    parser.add_argument("--cadence-ms", type=int, default=250)
    parser.add_argument("--replicas", type=int, default=4)
    parser.add_argument("--senders", type=int, default=32)
    parser.add_argument("--drain", type=int, default=16)
    parser.add_argument(
        "--mode", choices=["payments", "hotspot_amm"], default="payments"
    )
    parser.add_argument("--client-cpus", default="0-3")
    parser.add_argument("--node-cpus", default="4-31")
    args = parser.parse_args()
    assert not args.output.exists()
    if args.workload_output and args.workload_output.exists():
        parser.error("the workload output already exists")
    args.scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.scratch) as tmp:
        root = Path(tmp).resolve()
        wrapper = root / "replica"
        launch = "taskset -c " + shlex.quote(args.node_cpus) + " " + shlex.quote(str(args.binary.resolve())) + ' "$@"'
        wrapper_lines = ["#!/bin/sh"]
        mount = None
        if args.device_accounting:
            mount = json.loads(subprocess.check_output(
                ["findmnt", "--json", "--target", root, "--output", "TARGET,SOURCE,FSTYPE,OPTIONS"]
            ))["filesystems"][0]
            if mount["fstype"] in ("tmpfs", "ramfs") or not mount["source"].startswith("/dev/"):
                raise SystemExit("device accounting requires a block-backed scratch directory")
            wrapper_lines += [
                'if [ "$1" = run ]; then',
                "  exec systemd-run --user --scope --quiet --expand-environment=no --property=IOAccounting=yes -- " + launch,
                "fi",
            ]
        wrapper_lines.append("exec " + launch)
        wrapper.write_text("\n".join(wrapper_lines) + "\n")
        wrapper.chmod(0o700)
        fund = root / "evidence"
        fund.mkdir()
        env = dict(
            os.environ,
            STEM_TEST_GNU_TIME=str(args.time.resolve()),
            STEM_TEST_EVIDENCE_ROOT=str(fund),
            STEM_TEST_CONTRACT_DIR=str(args.contracts.resolve()),
            STEM_TEST_REPLICA_BIN=str(wrapper),
            STEM_STORAGE_RATE=str(args.rate),
            STEM_STORAGE_COMPLETE_VOLUME=str(int(args.complete_volume)),
            STEM_STORAGE_SECONDS=str(args.seconds),
            STEM_STORAGE_CADENCE=str(args.cadence_ms),
            STEM_STORAGE_REPLICAS=str(args.replicas),
            STEM_STORAGE_SENDERS=str(args.senders),
            STEM_STORAGE_PAYMENT_CHAINS=str(args.payment_chains),
            STEM_STORAGE_PREFUND_PAYMENTS=str(int(args.prefund_payments)),
            STEM_STORAGE_PAYMENT_DATA_BYTES=str(args.data_bytes),
            STEM_STORAGE_DRAIN=str(args.drain),
        )
        if args.device_accounting:
            env["STEM_TEST_DEVICE_ACCOUNTING"] = "1"
        case = "storage_load::sustained_payment_storage"
        if args.mode != "payments":
            env["STEM_LINEAGE_WORKLOAD"] = args.mode
            case = "storage_load::sustained_lineage_load"
        binary_hash, harness_hash = digest(args.binary), digest(args.harness)
        proc = subprocess.run(
            [
                "taskset",
                "-c",
                args.client_cpus,
                args.harness.resolve(),
                case,
                "--exact",
                "--ignored",
                "--nocapture",
            ],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        paths = list(fund.glob("stem-storage-*"))
        archive = dict(
            exit=proc.returncode,
            log=proc.stdout,
            parameters={k: str(v) for k, v in vars(args).items()},
            binary_sha256=binary_hash,
            harness_sha256=harness_hash,
        )
        if mount is not None:
            archive["scratch_mount"] = mount
        if len(paths) == 1:
            folder = paths[0]
            if (folder / "before.json").exists():
                archive["before_file_mtime_ns"] = (folder / "before.json").stat().st_mtime_ns
            for name in (
                "before.json",
                "report.json",
                "history-index-proof.json",
                "lineage-audit.json",
                "payment-funding.json",
            ):
                path = folder / name
                if path.exists():
                    archive[name] = json.loads(path.read_text())
            for name in ("workload.bin", "genesis.json"):
                path = folder / name
                if path.exists():
                    archive[name + "_sha256"] = digest(path)
            if args.workload_output and (folder / "workload.bin").is_file():
                with tempfile.NamedTemporaryFile(dir=args.workload_output.parent) as copy:
                    with (folder / "workload.bin").open("rb") as source:
                        shutil.copyfileobj(source, copy)
                    copy.flush()
                    assert digest(Path(copy.name)) == archive["workload.bin_sha256"]
                    os.link(copy.name, args.workload_output)
            if (folder / "node-0/db/LOG").exists():
                archive["sst"] = json.loads(
                    subprocess.check_output(
                        [sys.executable, ROOT / "tools/rocksdb_sst_report.py", folder]
                    )
                )
            if (folder / "index-process.stderr").exists():
                archive["index_process"] = (folder / "index-process.stderr").read_text()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with lzma.open(args.output, "wt", preset=3) as f:
            json.dump(archive, f, separators=(",", ":"))
        with lzma.open(args.output, "rt") as f:
            recorded = json.load(f)
        assert recorded["exit"] == proc.returncode and recorded.get(
            "workload.bin_sha256"
        ) == archive.get("workload.bin_sha256")
        if proc.returncode or "report.json" not in recorded:
            raise SystemExit("Load run failed; inspect the archived log")
        summary = recorded["report.json"]["summary"]
        print(json.dumps(summary, sort_keys=True))
        if args.device_accounting:
            device_writes(recorded["report.json"])
        if args.complete_volume:
            assert (
                summary["planned"]
                == summary["offered"]
                == summary["admitted"]
                == summary["visible"]
            )


if __name__ == "__main__":
    main()

"""Run sequential, matched reference/candidate/mixed localnets and keep public evidence."""

import argparse
import hashlib
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser()
    for name in ("reference", "candidate", "lock", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--sizes", nargs="+", type=int, default=[4, 16, 64, 128])
    parser.add_argument("--port", type=int, default=16200)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for n in args.sizes:
        modes = (
            ["reference", "candidate", "mixed"]
            if n in (4, 64)
            else ["candidate", "reference", "mixed"]
        )
        for mode in modes:
            folder = args.output / f"{n}-{mode}"
            port = args.port + 512 * args.sizes.index(n)
            binary = args.reference if mode == "reference" else args.candidate
            cmd = [
                sys.executable,
                "tools/jolteon_localnet.py",
                "--binary",
                str(binary),
                "--lock",
                str(args.lock),
                "--output",
                str(folder),
                "--port",
                str(port),
                "--validators",
                str(n),
                "--transactions",
                str(min(64, 2 * n + 8)),
                "--offline",
                str(1 if n == 4 else 2),
                "--offline-blocks",
                "2",
                "--offered",
                "20",
                "--recovery-seconds",
                "100",
            ]
            if n >= 64:
                cmd += ["--cadence", "700", "--timeout", "6000"]
            if mode == "mixed":
                cmd += ["--alternate-binary", str(args.reference)]
            with (args.output / (folder.name + ".log")).open("w") as log:
                code = subprocess.run(
                    cmd, stdout=log, stderr=subprocess.STDOUT
                ).returncode
            row = dict(n=n, mode=mode, exit=code, cmd=cmd)
            if (folder / "signed-workload.json").exists():
                row.update(
                    workload_sha256=digest(folder / "signed-workload.json"),
                    genesis_sha256=digest(folder / "genesis.json"),
                )
            if code == 0:
                row["report"] = json.loads((folder / "jolteon-report.json").read_text())
            else:
                row["failure_tail"] = (
                    args.output / (folder.name + ".log")
                ).read_text()[-2600:]
            results.append(row)
            print(n, mode, code, flush=True)
            record = dict(
                affinity=sorted(os.sched_getaffinity(0)),
                reference_sha256=digest(args.reference),
                candidate_sha256=digest(args.candidate),
                lock_sha256=digest(args.lock),
                runs=results,
            )
            (args.output / "public.json").write_text(
                json.dumps(record, sort_keys=True) + "\n"
            )
            # The verifier only needs public genesis, workload, proof and report.
            # Release this invocation's databases as soon as all nodes have exited.
            for database in folder.glob("node[0-9]*"):
                if database.is_dir():
                    shutil.rmtree(database)
        hashes = {
            (r["workload_sha256"], r["genesis_sha256"])
            for r in results
            if r["n"] == n and "workload_sha256" in r
        }
        assert len(hashes) == 1


if __name__ == "__main__":
    main()

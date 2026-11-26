"""Validate saved public inputs and save reports before discarding localnet databases."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path
import subprocess


def pack(root, binary, output):
    matrix = json.loads((root / "public.json").read_text())
    for row in matrix["runs"]:
        run = root / f"{row['n']}-{row['mode']}"
        for field, file in [
            ("workload_sha256", "signed-workload.json"),
            ("genesis_sha256", "genesis.json"),
        ]:
            assert hashlib.sha256((run / file).read_bytes()).hexdigest() == row[field]
        row["genesis"] = json.loads((run / "genesis.json").read_text())
        if row["exit"]:
            path = run / "last-status.json"
            if path.exists():
                row["last_status"] = json.loads(path.read_text())
            continue
        verified = json.loads(
            subprocess.check_output(
                [
                    binary,
                    "jolteon-proof",
                    "--genesis",
                    run / "genesis.json",
                    "--proof",
                    run / "two-chain.json",
                ]
            )
        )
        report = row["report"]
        assert verified == report["proof_verification"]
        assert verified["height"] >= report["agreement"]["height"]
        if verified["height"] == report["agreement"]["height"]:
            assert verified == report["agreement"]
        assert all(
            v["committed_transactions"] == report["payments"] for v in report["resumed"]
        )
    with lzma.open(output, "wt") as out:
        json.dump(matrix, out, sort_keys=True)
    with lzma.open(output, "rt") as saved:
        assert json.load(saved) == matrix


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for key in ("directory", "verifier", "output"):
        parser.add_argument(key, type=Path)
    args = parser.parse_args()
    pack(args.directory, args.verifier, args.output)

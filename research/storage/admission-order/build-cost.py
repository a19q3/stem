"""Run and validate the fixed-work proposal measurements on a fresh database in each process."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from statistics import median
import subprocess

ORDERS = ("wire_hash", "input_index", "parent_hash")


def run(binary, contracts):
    args = [binary.resolve(), "--exact", "admission_order::proposal_order_cost", "--nocapture"]
    result = subprocess.run(args, capture_output=True, text=True, check=True,
                            env={**os.environ, "STEM_TEST_CONTRACT_DIR": str(contracts.resolve())})
    assert "1 passed; 0 failed" in result.stdout
    encoded, = [line.removeprefix("PROPOSAL_ORDER ") for line in result.stdout.splitlines()
                if line.startswith("PROPOSAL_ORDER ")]
    samples = json.loads(encoded)
    assert len(samples) == 84
    for i in range(14):
        jobs = [s for s in samples if s["repetition"] == i]
        assert {s["position"] for s in jobs} == set(range(6))
        assert {(s["order"], s["spent_before_each_payment"]) for s in jobs} == {(name, spent) for name in ORDERS for spent in (False, True)}
    for name in ORDERS:
        assert len({s["header"] for s in samples if s["order"] == name}) == 1
    return samples


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "contracts", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    args = parser.parse_args()
    runs = [run(args.binary, args.contracts) for _ in range(2)]
    assert [(s["order"], s["header"]) for s in runs[0]] == [(s["order"], s["header"]) for s in runs[1]]
    with args.binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    data = {"cpus": sorted(os.sched_getaffinity(0)), "binary_sha256": digest, "warmups": 2,
            "vm_workers": 2, "live_payments_per_block": 2048, "runs": runs}
    with args.output.open("x") as out:
        json.dump(data, out, separators=(",", ":"))
        out.write("\n")
    for name in ORDERS:
        for spent in (False, True):
            centers = [median(s["build_ns"] for s in rows if s["repetition"] >= 2 and
                              s["order"] == name and s["spent_before_each_payment"] == spent) for rows in runs]
            print(name, spent, [round(n / 1e6, 3) for n in centers])


if __name__ == "__main__":
    main()

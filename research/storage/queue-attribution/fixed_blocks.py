"""Measure or summarize the two-worker fixed-block comparison."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from statistics import median
import subprocess


def summarize(trials):
    if [r["variant"] for r in trials] != ["reference", "candidate", "candidate", "reference"]:
        raise ValueError("expected the two counterbalanced pairs")
    blocks = trials[0]["blocks"]
    if [r["transactions"] for r in blocks] != [256, 1024, 2048]:
        raise ValueError("unexpected block sizes")
    for i, block in enumerate(blocks):
        medians = []
        for trial in trials:
            row = trial["blocks"][i]
            samples = row["prepare_and_finish_us"]
            if ((row["transactions"], row["header"]) != (block["transactions"], block["header"])
                    or len(samples) != 10 or any(not (total >= finish > 0) for total, finish in samples)):
                raise ValueError("fixed candidate Header or timing samples disagree")
            medians.append(tuple(median(col) for col in zip(*samples, strict=True)))
        print(block["transactions"], "median (prepare, finish) µs in run order:", *medians)


def measure(args):
    trials = []
    for name in ("reference", "candidate", "candidate", "reference"):
        exe = getattr(args, name).resolve()
        run = subprocess.run([exe, "--exact", "fixed_blocks::time_identical_funded_candidates", "--nocapture"],
                             check=True, text=True, stdout=subprocess.PIPE,
                             env={**os.environ, "STEM_TEST_CONTRACT_DIR": str(args.contracts.resolve())})
        line, = [s for s in run.stdout.splitlines() if s.startswith("FIXED_BLOCKS ")]
        with exe.open("rb") as f:
            digest = hashlib.file_digest(f, "sha256").hexdigest()
        trials.append({"variant": name, "harness_sha256": digest,
                       "blocks": json.loads(line.removeprefix("FIXED_BLOCKS "))})
    summarize(trials)
    with args.output.open("x") as f:
        json.dump({"logical_cpus": sorted(os.sched_getaffinity(0)), "warmup": 2, "trials": trials}, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    cmds = parser.add_subparsers(dest="command", required=True)
    cmds.add_parser("summary").add_argument("archive", type=Path)
    run = cmds.add_parser("measure")
    for arg in ("reference", "candidate", "contracts", "output"):
        run.add_argument("--" + arg, type=Path, required=True)
    args = parser.parse_args()
    if args.command == "measure":
        measure(args)
    else:
        summarize(json.loads(args.archive.read_bytes())["trials"])

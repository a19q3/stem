"""Measure identical signed child blocks against committed and speculative parents."""
import argparse
import hashlib
import json
import os
from pathlib import Path
from statistics import median
import subprocess

PHASES = ("node_reads", "speculative_node_reads", "stored_node_bytes", "node_writes",
          "cell_hash_ns", "sort_ns", "edit_ns", "node_read_ns", "pack_ns", "system_ns", "receipts_and_history_ns")


def run(binary, contracts, diagnostics):
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = subprocess.run([binary.resolve(), "--exact", "patricia_finishing::measure_fixed_children", "--nocapture"],
                            capture_output=True, text=True, check=True,
                            env={**os.environ, "STEM_TEST_CONTRACT_DIR": str(contracts.resolve())})
    assert "1 passed; 0 failed" in result.stdout
    samples, counters = [], None
    for line in result.stdout.splitlines():
        if line.startswith("FINISH "):
            counters = list(map(int, line.split()[1:]))
            assert len(counters) == len(PHASES)
        if line.startswith("CHILD "):
            _, rep, size, branch, header, elapsed = line.split()
            row = {"repetition": int(rep), "transactions": int(size), "parent": "speculative" if branch == "true" else "committed",
                   "header": header, "prepare_ns": int(elapsed)}
            assert branch in ("false", "true") and (counters is not None) == diagnostics
            if diagnostics:
                row.update(zip(PHASES, counters, strict=True))
                assert row["edit_ns"] >= row["node_read_ns"]
            samples.append(row)
            counters = None
    expected = {(rep, size, parent) for rep in range(14) for size in (256, 1024, 2048)
                for parent in ("speculative", "committed")}
    assert len(samples) == len(expected) and {(r["repetition"], r["transactions"], r["parent"]) for r in samples} == expected
    return {"binary_sha256": digest, "samples": samples}


def summarize(data):
    variants = [r["variant"] for r in data["runs"]]
    assert variants in (["diagnostic"], ["reference", "candidate", "candidate", "reference"])
    for size in (256, 1024, 2048):
        assert len({r["header"] for run in data["runs"] for r in run["samples"] if r["transactions"] == size}) == 1
        for parent in ("committed", "speculative"):
            values = []
            for run in data["runs"]:
                rows = [r for r in run["samples"] if r["transactions"] == size and r["parent"] == parent and r["repetition"] >= 2]
                assert len(rows) == 12
                values.append(round(median(r["prepare_ns"] for r in rows)/1e6, 3))
            print(size, parent, "median preparation milliseconds", *values)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    cmds = parser.add_subparsers(dest="action", required=True)
    cmds.add_parser("summary").add_argument("archive", type=Path)
    for action in ("diagnostic", "compare"):
        cmd = cmds.add_parser(action)
        for name in (("diagnostic",) if action == "diagnostic" else ("reference", "candidate")) + ("contracts", "output"):
            cmd.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    if args.action == "summary":
        summarize(json.loads(args.archive.read_bytes()))
        return
    diagnostic = args.action == "diagnostic"
    order = ("diagnostic",) if diagnostic else ("reference", "candidate", "candidate", "reference")
    data = {"cpus": sorted(os.sched_getaffinity(0)), "warmups": 2, "runs": []}
    for variant in order:
        data["runs"].append({"variant": variant, **run(getattr(args, variant), args.contracts, diagnostic)})
    summarize(data)
    with args.output.open("x") as output:
        json.dump(data, output, separators=(",", ":"))
        output.write("\n")


if __name__ == "__main__":
    main()

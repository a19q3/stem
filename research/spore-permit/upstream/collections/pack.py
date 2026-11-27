"""Collect public verification inputs from the six completed localnet runs."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess

CASES = ("ordinary", "custody", "immortal", "registered", "plain", "import")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=Path, required=True)
    parser.add_argument("--stem", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    evidence, trust = {}, {}
    for case in CASES:
        directory = args.runs / case
        report = json.loads((directory / "jolteon-bridge-report.json").read_text())
        history = report["spore_history"]
        record = {**history, "settlement": report["spore_settlement"],
                  "agreed": report["agreed"], "replicas": report["replicas"]}
        for label, target in [("spore", record), ("collection", record.get("collection"))]:
            if target is not None:
                target["proof"] = json.loads((directory / f"{label}-history-proof.json").read_text())
        evidence[case] = record
        trust[case] = json.loads(subprocess.check_output(
            [args.stem.resolve(), "jolteon-trust", "--genesis", directory / "stem.json"]))
    args.output.mkdir(parents=True, exist_ok=True)
    with lzma.open(args.output / "settlements.json.xz", "xt") as out:
        json.dump(evidence, out, separators=(",", ":"))
    with (args.output / "committee-trust.json").open("x") as out:
        json.dump(trust, out, indent=2)
        out.write("\n")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Export public browser test evidence from a CKB-backed Jolteon stake run."""

import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--run", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    genesis = args.run / "stem-genesis.json"

    def command(*parts):
        return json.loads(subprocess.check_output([args.binary, *parts], text=True))

    trust = command("jolteon-trust", "--genesis", genesis)
    prior = []
    changes = []
    epoch = 0
    while (path := args.run / f"handoff-{epoch}.json").is_file():
        header = command("jolteon-proof", "--genesis", genesis, "--proof", path, "--terminal", *prior)
        changes.append({"header": header, "proof": json.loads(path.read_text())})
        prior.extend(("--handoff", path))
        epoch += 1
    if not changes:
        raise RuntimeError("The stake run must contain a CKB-backed handoff")
    final = args.run / "post-handoff.json"
    successor = command("jolteon-proof", "--genesis", genesis, "--proof", final, *prior)
    args.output.write_text(json.dumps({"trust": trust, "changes": changes, "successor": {
        "header": successor, "finality": json.loads(final.read_text()),
    }}) + "\n")


if __name__ == "__main__":
    main()

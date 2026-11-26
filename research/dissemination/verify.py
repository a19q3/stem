"""Compare all deterministic outputs of a fresh run with the archived experiment."""

import json
import lzma
from pathlib import Path
import sys


def normalized(run):
    for row in run["records"]:
        geometry = row["geometry"]
        geometry.pop("build_seconds")
        for key in ("second", "least", "expansion_lower_spectral"):
            geometry[key] = round(geometry[key], 7)
    return run


if __name__ == "__main__":
    with lzma.open(Path(__file__).with_name("results.json.xz"), "rt") as archived:
        expected = normalized(json.load(archived))
    with open(sys.argv[1]) as replay:
        actual = normalized(json.load(replay))
    if actual != expected:
        raise SystemExit("Replay differs from the archived matrix")
    print(f"Matched {sum(len(r['traffic']) for r in actual['records'])} traffic rows")

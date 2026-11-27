"""Retain the background admission counters and process ticks from a measured run."""
import argparse
import json
import lzma
from pathlib import Path
import shutil


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("report", "cpu", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    with args.report.open() as source:
        measured = json.load(source)
    snapshots = {phase: [status.get("admission_pipeline") for status in measured[phase]["statuses"]]
                 for phase in ("before", "after")}
    if any(len(s) != 4 for s in snapshots.values()):
        raise ValueError("expected four validator status samples")
    with (args.output / "pipeline.json").open("x") as archive:
        json.dump(snapshots, archive, indent=2)
        archive.write("\n")
    with args.cpu.open("rb") as source, lzma.open(args.output / "cpu.json.xz", "xb") as archive:
        shutil.copyfileobj(source, archive)


if __name__ == "__main__":
    main()

"""Extract repeatable measurements from one completed local four-validator trial."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
for folder in (HERE.parent / "dispatch", HERE.parent / "external", HERE.parents[2] / "tools"):
    sys.path.insert(0, str(folder))
from counters import summarize as node_counters
from pack import pack
from rocksdb_sst_report import report as sst_report


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write(path, value):
    with lzma.open(path, "xt") as target:
        json.dump(value, target, separators=(",", ":"))


def collect(directory, cpu, device, archive):
    raw, = (directory / "measurement").glob("stem-storage-*/report.json")
    with raw.open() as source:
        trial = json.load(source)
    before, after = trial["before"]["storage"], trial["after"]["storage"]
    deltas = [
        {key: stop["rocksdb"][key] - value for key, value in start["rocksdb"].items()}
        for start, stop in zip(before, after, strict=True)
    ]
    if len(deltas) != 4 or any(value < 0 for delta in deltas for value in delta.values()):
        raise ValueError("expected four monotonic RocksDB snapshots")
    tables = []
    for name, rows in sst_report(directory)["logs"].items():
        for item in rows:
            tables.append([
                name.split("/")[0], item["time_micros"], item["job"], item["file_number"],
                item["kind"], item["file_size"],
            ])
    if len({(r[0], r[3]) for r in tables}) != len(tables):
        raise ValueError("duplicate SST file creation on a replica")
    observed = {
        "raw_sha256": sha(raw),
        "genesis_id": json.loads((directory / "trial/genesis-id.json").read_text()),
        "preparation": json.loads((directory / "trial/trial.json").read_text()),
        "driver": json.loads((directory / "measurement/driver.json").read_text()),
        "counters": node_counters(trial),
        "rocksdb_delta": deltas,
        "sst": tables,
        "cpu": json.loads(cpu.read_bytes()),
        "device": json.loads(device.read_bytes()),
    }
    write(archive, pack(trial))
    observed["archive_sha256"] = sha(archive)
    return observed


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("trial_directory", "cpu", "device", "payment_archive", "observation_archive"):
        parser.add_argument(key, type=Path)
    args = parser.parse_args()
    data = collect(args.trial_directory, args.cpu, args.device, args.payment_archive)
    write(args.observation_archive, {args.trial_directory.name: data})

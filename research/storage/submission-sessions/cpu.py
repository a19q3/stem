"""Observe CPU ticks for a local launcher with four nodes and one external measurement driver."""
import argparse
from pathlib import Path
import json
import os
import time


def processes(trial):
    nodes, drivers = {}, []
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            comm = (path / "comm").read_text().strip()
            if comm != "stem" and not comm.startswith("native_store-"):
                continue
            args = (path / "cmdline").read_bytes().decode(errors="replace").rstrip("\0").split("\0")
            if not args:
                continue
            if Path(args[0]).name == "stem" and "--home" in args:
                home = Path(args[args.index("--home") + 1])
                if home.parent == trial and home.name in {f"node-{i}" for i in range(4)}:
                    nodes[home.name] = path
            elif Path(args[0]).name.startswith("native_store-") and "storage_load::sustained_payment_storage" in args:
                drivers.append(path)
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    if len(nodes) != 4:
        return {}
    try:
        groups = {(path / "cgroup").read_bytes() for path in nodes.values()}
        matches = [path for path in drivers if (path / "cgroup").read_bytes() in groups]
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return {}
    if len(groups) == 1 and len(matches) == 1:
        return {**nodes, "driver": matches[0]}
    return {}


def read_ticks(path):
    fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
    return int(fields[19]), int(fields[11]) + int(fields[12])


def capture(trial, output, timeout):
    deadline = time.monotonic() + timeout
    while True:
        pids = processes(trial)
        if pids:
            try:
                births = {name: read_ticks(path)[0] for name, path in pids.items()}
                break
            except (FileNotFoundError, ProcessLookupError):
                pass
        if time.monotonic() >= deadline:
            raise TimeoutError(f"measurement processes did not start for {trial}")
        time.sleep(0.5)
    samples = []
    while True:
        start = time.time_ns() // 1000
        try:
            current = {name: read_ticks(path) for name, path in pids.items()}
        except (FileNotFoundError, ProcessLookupError):
            break
        if any(current[k][0] != births[k] for k in births):
            break
        samples.append([start, time.time_ns() // 1000, {k: current[k][1] for k in current}])
        time.sleep(0.25)
    with output.open("x") as destination:
        json.dump({"ticks_per_second": os.sysconf("SC_CLK_TCK"), "samples": samples}, destination,
                  separators=(",", ":"))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trial", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--startup-timeout", type=int, default=120)
    args = parser.parse_args()
    capture(args.trial.resolve(), args.output, args.startup_timeout)

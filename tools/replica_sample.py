#!/usr/bin/env python3
"""Record Linux process and allocated storage samples on one benchmark host."""

import argparse
import json
import math
from pathlib import Path
import platform
import time

from localnet import allocated_storage, process_resources


def process_start(pid):
    return int((Path("/proc") / str(pid) / "stat").read_text().rsplit(") ", 1)[1].split()[19])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", required=True)
    parser.add_argument("--app-pid", type=int, required=True)
    parser.add_argument("--comet-pid", type=int, required=True)
    parser.add_argument("--app-home", type=Path, required=True)
    parser.add_argument("--comet-home", type=Path, required=True)
    parser.add_argument("--seconds", type=float, required=True)
    parser.add_argument("--interval", type=float, default=1)
    args = parser.parse_args()
    comet_data = args.comet_home / "data"
    if (not args.app_home.is_dir() or not comet_data.is_dir() or args.app_pid <= 0 or args.comet_pid <= 0 or
            args.app_pid == args.comet_pid or any(not math.isfinite(value) or value <= 0 for value in
                                                 (args.seconds, args.interval))):
        parser.error("Provide live distinct processes, data directories and positive finite durations")
    pids = {"application": args.app_pid, "cometbft": args.comet_pid}
    identities = {name: process_start(pid) for name, pid in pids.items()}
    begin = time.monotonic()
    sample_after = begin
    while True:
        now = time.monotonic()
        final = now >= begin + args.seconds
        sample = {"label": args.label, "unix_ns": time.time_ns(), "kernel": platform.release(), "processes": {}}
        for name, pid in pids.items():
            try:
                if process_start(pid) != identities[name]:
                    raise ProcessLookupError(f"PID {pid} was reused")
                sample["processes"][name] = {"pid": pid, **process_resources(pid)}
            except (FileNotFoundError, ProcessLookupError) as error:
                sample["processes"][name] = {"pid": pid, "unavailable": str(error)}
        if sample_after == begin or final:
            sample["allocated_bytes"] = allocated_storage(args.app_home, comet_data)
        print(json.dumps(sample, separators=(",", ":")), flush=True)
        if final:
            return
        sample_after = min(sample_after + args.interval, begin + args.seconds)
        time.sleep(max(0, sample_after - time.monotonic()))


if __name__ == "__main__":
    main()

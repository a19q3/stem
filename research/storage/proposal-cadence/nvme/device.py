"""Sample Linux cgroup v2 writes and bracket a payment trial's offer and drain."""

import argparse
import bisect
import itertools
import json
import lzma
from pathlib import Path
import subprocess
import time


def capture(output, command):
    if not command:
        raise ValueError("supply the command that starts the driver and all four validators")
    unit = f"stem-payment-{time.monotonic_ns()}.scope"
    samples = []
    counter = None
    with output.open("x") as target:
        process = subprocess.Popen([
            "systemd-run", "--user", "--scope", "--quiet", "--expand-environment=no",
            "--property=IOAccounting=yes", "--unit=" + unit, "--", *command,
        ])
        try:
            while process.poll() is None:
                if counter is None:
                    lookup = subprocess.run(["systemctl", "--user", "show", "--property=ControlGroup", "--value", unit],
                                            capture_output=True, text=True)
                    group = lookup.stdout.strip()
                    if group:
                        counter = Path("/sys/fs/cgroup") / group.lstrip("/") / "io.stat"
                if counter is not None:
                    try:
                        raw = counter.read_text()
                    except FileNotFoundError:
                        counter = None
                    else:
                        samples.append([time.time_ns() // 1000, raw])
                time.sleep(0.25)
        finally:
            if process.poll() is None:
                subprocess.run(["systemctl", "--user", "stop", unit], check=False)
            status = process.wait()
        json.dump(samples, target, separators=(",", ":"))
    if status:
        raise SystemExit(status)
    if not samples:
        raise ValueError("the transient scope returned no device samples")


def writes(raw):
    result = {}
    for line in raw.splitlines():
        device, *metrics = line.split()
        counters = dict(field.split("=", 1) for field in metrics)
        result[device] = int(counters["wbytes"])
    return result


def bracket(samples, timestamps, target):
    index = bisect.bisect_left(timestamps, target)
    if index == 0 or index == len(samples):
        raise ValueError("device samples do not bracket the measurement")
    before, after = samples[index - 1:index + 1]
    return writes(before[1]), writes(after[1]), [target - before[0], after[0] - target]


def bounds(left, right):
    before_start, after_start, _ = left
    before_end, after_end, _ = right
    values = {}
    for device in sorted(before_start.keys() | after_end.keys()):
        low = before_end.get(device, 0) - after_start.get(device, 0)
        high = after_end.get(device, 0) - before_start.get(device, 0)
        if not 0 <= low <= high:
            raise ValueError("nonmonotonic scope write counter")
        values[device] = [low, high]
    return values


def summarize(trial, samples):
    timestamps = [unix_us for unix_us, _ in samples]
    if any(a > b for a, b in itertools.pairwise(timestamps)):
        raise ValueError("cgroup sampling clock regressed")
    clock = trial["driver_clock"]["start"]
    epoch = round(clock["unix_us"] - (clock["before_us"] + clock["after_us"]) / 2)
    end = epoch + trial["load"]["seconds"] * 1_000_000
    stop = end + trial["load"]["drain"] * 1_000_000
    left, middle, right = (bracket(samples, timestamps, t) for t in (epoch, end, stop))
    return {
        "sample_count": len(samples), "window_wbytes_bounds": bounds(left, middle),
        "drain_wbytes_bounds": bounds(middle, right), "total_wbytes_bounds": bounds(left, right),
        "boundary_distance_us": [b[2] for b in (left, middle, right)],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="mode", required=True)
    record = commands.add_parser("capture")
    record.add_argument("--output", type=Path, required=True)
    record.add_argument("command", nargs=argparse.REMAINDER)
    replay = commands.add_parser("summarize")
    replay.add_argument("--samples", type=Path, required=True)
    replay.add_argument("trials", type=Path, nargs="+")
    args = parser.parse_args()
    if args.mode == "capture":
        command = args.command[1:] if args.command[:1] == ["--"] else args.command
        capture(args.output, command)
    else:
        with lzma.open(args.samples, "rt") as source:
            samples = json.load(source)
        results = {}
        for path in args.trials:
            with lzma.open(path, "rt") as source:
                results[path.name] = summarize(json.load(source), samples[path.name])
        print(json.dumps(results, indent=2))

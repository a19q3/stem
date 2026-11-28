"""Capture local validator resource evidence for a paired state-engine run."""

import hashlib
import ipaddress
import json
import os
from pathlib import Path
import sys

if __package__:
    from .qualify import parse_endpoint
else:
    from qualify import parse_endpoint

VALUE_FLAGS = {
    "home", "genesis", "configuration", "key-file", "history-window",
    "rocksdb-block-cache-mib", "live-cell-index-mib",
    "live-cell-recovery-workers", "cell-node-index-mib",
}
SWITCHES = {"initialize", "archive", "storage-metrics", "live-cell-index-intern-scripts"}


def file_digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run_arguments(argv):
    if len(argv) < 2 or argv[1] != "run":
        raise ValueError("validator must be a Stem run process")
    args = {}
    tokens = iter(argv[2:])
    for token in tokens:
        name, separator, value = token.removeprefix("--").partition("=")
        if not token.startswith("--") or name in args:
            raise ValueError("unexpected or repeated validator argument")
        if name in VALUE_FLAGS:
            value = value if separator else next(tokens, "")
            if not value or (not separator and value.startswith("--")):
                raise ValueError(f"missing value for --{name}")
            args[name] = value
        elif name in SWITCHES and not separator:
            args[name] = True
        else:
            raise ValueError("unexpected validator argument")
    if not {"home", "genesis", "configuration", "key-file"} <= args.keys():
        raise ValueError("missing required validator arguments")
    return args


def proc_tcp_address(encoded):
    address, port = encoded.split(":")
    # Linux shows each address word in native byte order in /proc/net/tcp{6}.
    raw = b"".join(int(address[i:i + 8], 16).to_bytes(4, sys.byteorder)
                   for i in range(0, len(address), 8))
    address = ipaddress.ip_address(raw)
    return getattr(address, "ipv4_mapped", None) or address, int(port, 16)


def assert_listener(proc, endpoint):
    address, port = parse_endpoint(endpoint)
    address = getattr(address, "ipv4_mapped", None) or address
    owned = set()
    for descriptor in (proc / "fd").iterdir():
        try:
            target = os.readlink(descriptor)
        except FileNotFoundError:
            continue
        if target.startswith("socket:[") and target.endswith("]"):
            owned.add(target[8:-1])
    for table in ("tcp", "tcp6"):
        try:
            rows = (proc / "net" / table).read_text().splitlines()
        except FileNotFoundError:
            continue
        for line in rows[1:]:
            fields = line.split()
            if fields[3] == "0A" and fields[9] in owned:
                bound_address, bound_port = proc_tcp_address(fields[1])
                if (bound_address, bound_port) == (address, port):
                    return
    raise ValueError(f"validator does not own the listening socket for {endpoint}")


def cpu_stat(proc):
    fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    return int(fields[19]), int(fields[11]) + int(fields[12])


def cgroup_limits(proc):
    line = next((line for line in (proc / "cgroup").read_text().splitlines()
                 if line.startswith("0::/")), None)
    if line is None:
        return None
    root = Path("/sys/fs/cgroup")
    group = Path(line.removeprefix("0::/"))
    if ".." in group.parts:
        raise ValueError("invalid validator cgroup path")
    path = root / group
    if not path.is_dir():
        return None
    levels = []
    while True:
        limits = {}
        for name in ("cpu.max", "cpuset.cpus.effective", "memory.max", "io.max"):
            try:
                lines = (path / name).read_text().splitlines()
            except FileNotFoundError:
                continue
            limits[name] = sorted(lines)
        levels.append(limits)
        if path == root:
            return levels
        path = path.parent


def thread_affinities(proc, main_affinity):
    masks = {tuple(main_affinity)}
    for thread in (proc / "task").iterdir():
        try:
            masks.add(tuple(sorted(os.sched_getaffinity(int(thread.name)))))
        except ProcessLookupError:
            continue
    return [list(mask) for mask in sorted(masks)]


def optional_mib(args, name):
    if name not in args:
        return None
    value = int(args[name])
    if value <= 0:
        raise ValueError(f"invalid --{name}")
    return value


def argument_path(proc, value):
    path = Path(value)
    if path.is_absolute():
        return proc / "root" / path.relative_to("/")
    return proc / "cwd" / path


def process_sample(pid, endpoint, trial, binaries):
    proc = Path("/proc") / str(pid)
    start_ticks, _ = cpu_stat(proc)
    argv = (proc / "cmdline").read_bytes().rstrip(b"\0").split(b"\0")
    args = run_arguments([os.fsdecode(arg) for arg in argv])
    assert_listener(proc, endpoint)
    if file_digest(argument_path(proc, args["genesis"])) != trial["genesis_sha256"]:
        raise ValueError("validator genesis file differs from the prepared trial")
    if file_digest(argument_path(proc, args["configuration"])) != trial["topology_sha256"]:
        raise ValueError("validator configuration differs from the prepared trial")

    exe = proc / "exe"
    stat = exe.stat()
    identity = (stat.st_dev, stat.st_ino, stat.st_mtime_ns, stat.st_size)
    if identity not in binaries:
        binaries[identity] = file_digest(exe)
    affinity = sorted(os.sched_getaffinity(pid))
    config = {
        "binary_sha256": binaries[identity],
        "cpu_affinity": affinity,
        "thread_cpu_affinities": thread_affinities(proc, affinity),
        "cgroup_limits": cgroup_limits(proc),
        "history_window": None if args.get("archive") else int(args.get("history-window", 256)),
        "rocksdb_block_cache_mib": optional_mib(args, "rocksdb-block-cache-mib"),
        "live_cell_index_mib": optional_mib(args, "live-cell-index-mib"),
        "live_cell_recovery_workers": int(args.get("live-cell-recovery-workers", 1)),
        "live_cell_index_intern_scripts": args.get("live-cell-index-intern-scripts", False),
        "cell_node_index_mib": optional_mib(args, "cell-node-index-mib"),
        "storage_metrics": args.get("storage-metrics", False),
    }
    status = dict(line.split(":", 1) for line in (proc / "status").read_text().splitlines())
    io = dict(line.split(":", 1) for line in (proc / "io").read_text().splitlines())
    end_ticks, cpu_ticks = cpu_stat(proc)
    if start_ticks != end_ticks:
        raise ValueError(f"validator PID changed while sampling {endpoint}")
    return {
        "endpoint": endpoint, "pid": pid, "start_ticks": start_ticks, "configuration": config,
        "cpu_ticks": cpu_ticks, "peak_rss_kib": int(status["VmHWM"].split()[0]),
        "read_bytes": int(io["read_bytes"]), "write_bytes": int(io["write_bytes"]),
    }


def sample_runtime(pids, endpoints, trial):
    if len(pids) != len(endpoints) or len(set(pids)) != len(pids) or any(pid <= 0 for pid in pids):
        raise ValueError("supply one distinct positive PID for every prepared endpoint in order")
    binaries = {}
    replicas = [process_sample(pid, endpoint, trial, binaries)
                for pid, endpoint in zip(pids, endpoints, strict=True)]
    if len(set(binaries.values())) != 1:
        raise ValueError("validator binaries differ within the same measurement")
    return {
        "cpu_ticks_per_second": os.sysconf("SC_CLK_TCK"),
        "driver_cpu_affinity": sorted(os.sched_getaffinity(0)),
        "replicas": replicas,
    }


def finish_runtime(before, after, report):
    result = {
        "version": 1,
        "report_sha256": file_digest(report),
        "cpu_ticks_per_second": before["cpu_ticks_per_second"],
        "driver_cpu_affinity": before["driver_cpu_affinity"],
        "replicas": [],
    }
    for key in ("cpu_ticks_per_second", "driver_cpu_affinity"):
        if before[key] != after[key]:
            raise ValueError(f"driver {key} changed during measurement")
    for start, end in zip(before["replicas"], after["replicas"], strict=True):
        for key in ("endpoint", "pid", "start_ticks", "configuration"):
            if start[key] != end[key]:
                raise ValueError(f"validator {key} changed during measurement")
        deltas = {key: end[key] - start[key] for key in ("cpu_ticks", "read_bytes", "write_bytes")}
        if min(deltas.values()) < 0 or end["peak_rss_kib"] < start["peak_rss_kib"]:
            raise ValueError("validator process counters regressed")
        result["replicas"].append({
            "endpoint": end["endpoint"], "configuration": end["configuration"],
            "process": {**deltas, "peak_rss_kib": end["peak_rss_kib"]},
        })
    return result


def read_runtime(path, report, endpoints):
    evidence = json.loads(path.read_bytes())
    if evidence["version"] != 1 or evidence["report_sha256"] != file_digest(report):
        raise ValueError("runtime evidence differs from the measurement report")
    if [parse_endpoint(node["endpoint"]) for node in evidence["replicas"]] != endpoints:
        raise ValueError("runtime evidence endpoints differ from the prepared committee")
    return evidence

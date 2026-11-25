#!/usr/bin/env python3
"""Run real Jolteon replicas with one leader's realtime clock offset."""

import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import time

from bridge_localnet import binary
from jolteon_localnet import FINALITY, HEADER, STATUS, SUBMIT, evidence_keys, genesis_and_payments, rpc, write_configuration
from localnet import run, write_json
from native_bridge_localnet import wait_for


CLOCK_SHIM = """\
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <time.h>

int clock_gettime(clockid_t clock, struct timespec *value) {
    int (*real_clock)(clockid_t, struct timespec *) = dlsym(RTLD_NEXT, "clock_gettime");
    if (!real_clock) abort();
    int result = real_clock(clock, value);
    const char *offset = getenv("STEM_FIXTURE_CLOCK_OFFSET");
    if (!result && clock == CLOCK_REALTIME && offset) {
        value->tv_sec += strtol(offset, NULL, 10);
    }
    return result;
}
"""


def scenario(args):
    source = args.output / "realtime-offset.c"
    source.write_text(CLOCK_SHIM)
    shim = source.with_suffix(".so")
    subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC", source, "-o", shim, "-ldl"], check=True)
    keys = evidence_keys(args)
    weights = [1] * args.validators
    spec, payments = genesis_and_payments(args, keys, weights)
    ports = list(range(args.port, args.port + args.validators))
    config = write_configuration(args.output, [key for key, _ in keys], ports, cadence=400, timeout=1600)
    nodes = {}

    def launch(index, skewed=False, initialize=True):
        home = args.output / f"node{index}"
        cmd = [args.binary, "run", "--home", home, "--genesis", spec, "--configuration", config,
               "--key-file", keys[index][1]]
        if initialize:
            cmd.append("--initialize")
        env = os.environ.copy()
        if skewed:
            env.update(LD_PRELOAD=str(shim), STEM_FIXTURE_CLOCK_OFFSET=str(args.offset_seconds))
        stage = "initial" if initialize else "restart"
        with (args.output / f"replica-{index}-{stage}.log").open("wb") as log:
            nodes[index] = subprocess.Popen(cmd, env=env, stdout=log, stderr=subprocess.STDOUT)

    def statuses(indices):
        for index in indices:
            if nodes[index].poll() is not None:
                raise AssertionError(f"replica {index} exited with {nodes[index].returncode}")
        return [rpc(ports[index], STATUS) for index in indices]

    def stop(index):
        process = nodes[index]
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=8)

    def header(height):
        replies = [rpc(port, HEADER, struct.pack("<Q", height)) for port in ports]
        if any(value != replies[0] for value in replies) or "header" not in replies[0]:
            raise AssertionError(replies)
        return replies[0]

    try:
        started_ms = time.time_ns() // 1_000_000
        for index in range(args.validators):
            launch(index, skewed=index == 0)

        def stabilized():
            healthy = statuses([1, 2, 3])
            return healthy if all(state["height"] >= 3 for state in healthy) else None

        healthy = wait_for(stabilized, "healthy quorum finalizing after a skewed leader", seconds=30)
        for state in healthy:
            rejected_view = state["last_untimely_view"]
            assert rejected_view > 0 and (rejected_view - 1) % args.validators == 0, healthy
            assert 1 in state["timeout_views"], healthy
        stop(0)
        launch(0, initialize=False)
        reply = rpc(ports[1], SUBMIT, binary(payments[0]))
        assert "admitted" in reply, reply

        def recovered():
            states = statuses(list(range(args.validators)))
            return states if all(state["committed_transactions"] >= 1 for state in states) else None

        resumed = wait_for(recovered, "the restarted leader and all other replicas committing payment", seconds=30)
        height = min(state["height"] for state in resumed)
        headers = [header(h) for h in range(1, height + 1)]
        timestamps = [struct.unpack_from("<Q", bytes.fromhex(record["header"]), 8)[0] for record in headers]
        assert timestamps == sorted(set(timestamps)), timestamps
        finished_ms = time.time_ns() // 1_000_000
        assert started_ms <= timestamps[0] and timestamps[-1] <= finished_ms, timestamps
        proof = rpc(ports[0], FINALITY, struct.pack("<Q", height))
        proof_path = args.output / "finality-proof.json"
        write_json(proof_path, proof["proof"])
        verified = json.loads(run(args.binary, "jolteon-proof", "--genesis", spec, "--proof", proof_path))
        assert verified["height"] >= height, verified
        report = {"offset_seconds": args.offset_seconds, "healthy": healthy, "resumed": resumed,
                  "headers": headers, "timestamps_ms": timestamps, "finality": verified,
                  "started_ms": started_ms, "finished_ms": finished_ms}
        write_json(args.output / "clock-report.json", report)
        print(f"Rejected {args.offset_seconds:+d}s leader in view 1, resumed payment finality at height {height}")
    finally:
        diagnostics = {}
        for index, process in nodes.items():
            diagnostics[index] = {"exit": process.poll()}
            if process.poll() is None:
                try:
                    diagnostics[index]["status"] = rpc(ports[index], STATUS)
                except (OSError, ValueError) as exc:
                    diagnostics[index]["error"] = str(exc)
        write_json(args.output / "last-status.json", diagnostics)
        for index in nodes:
            stop(index)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "lock", "output"):
        parser.add_argument("--" + flag, type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--offset-seconds", type=int, choices=(-120, 120), required=True)
    parser.add_argument("--port", type=int, default=22500)
    args = parser.parse_args()
    args.validators = 4
    args.transactions = 1
    args.output.mkdir(mode=0o700, parents=True)
    scenario(args)


if __name__ == "__main__":
    main()

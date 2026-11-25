#!/usr/bin/env python3
"""Benchmark signed CKB transactions against independently provisioned sidechain replicas."""

import argparse
import base64
import concurrent.futures as futures
import hashlib
import json
import math
from pathlib import Path
import platform
import threading
import time

from localnet import (COMET_METRICS, Rpc, export_execution_proof, http_endpoint, observe, prefund,
                      prometheus_metrics, run, summarize, write_json)

MAX_WORKLOAD_BYTES = 256 << 20
PROCESS_COUNTERS = ("process_cpu_seconds_total", "process_resident_memory_bytes", "process_start_time_seconds")


def read_json(path, limit):
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"Input exceeds size limit: {path}")
    return json.loads(data)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Endpoint:
    def __init__(self, args, number, entry):
        self.args = args
        self.number = number
        self.rpc = entry["rpc"]
        self.metrics = entry.get("metrics")
        self.submit = entry.get("submit", True)
        self.label = entry["label"]
        self.host = entry["host"]
        if not (isinstance(self.submit, bool) and self.label and self.host):
            raise ValueError("Provide a label, host and boolean submission policy")
        http_endpoint(self.rpc)
        if self.metrics is not None:
            http_endpoint(self.metrics)

    def resources(self):
        # Process I/O and combined application plus consensus CPU are host-local.
        return {}

    def telemetry(self):
        if self.metrics is None:
            return {}
        return prometheus_metrics(self.metrics, COMET_METRICS + PROCESS_COUNTERS, timeout=2)


def application_query(rpc, path):
    result = rpc.call("abci_query", path=json.dumps(path))["response"]
    if result["code"] != 0:
        raise RuntimeError(result)
    return json.loads(base64.b64decode(result["value"]))


def probe(node, chain, genesis_hash, include_storage=False):
    rpc = Rpc(node.rpc)
    try:
        status = rpc.call("status")
        app = application_query(rpc, "/tip")
        if (status["node_info"]["network"] != chain or app["chain_id"] != chain or
                app["genesis_hash"] != genesis_hash):
            raise RuntimeError(f"{node.label} serves a different chain or application genesis")
        sync = status["sync_info"]
        result = {"node_id": status["node_info"]["id"], "comet_version": status["node_info"]["version"],
                  "validator": status["validator_info"],
                  "application_height": app["height"], "application_hash": app["app_hash"],
                  "consensus_height": int(sync["latest_block_height"]), "consensus_base": int(sync["earliest_block_height"]),
                  "block_hash": sync["latest_block_hash"], "catching_up": sync["catching_up"],
                  "application_metrics": application_query(rpc, "/metrics"), "prometheus": node.telemetry()}
        if include_storage:
            result["logical_storage"] = application_query(rpc, "/storage_stats")
        return result
    finally:
        rpc.close()


def common_block(nodes, tips):
    height = min(tip["consensus_height"] for tip in tips)
    if height < max(1, *(tip["consensus_base"] for tip in tips)):
        raise RuntimeError("Replicas have no common retained block height")
    hashes = set()
    for node in nodes:
        rpc = Rpc(node.rpc)
        try:
            hashes.add(rpc.call("block", height=height)["block_id"]["hash"])
        finally:
            rpc.close()
    if len(hashes) != 1:
        raise RuntimeError(f"Replicas disagree on the committed block at height {height}")
    return {"height": height, "hash": hashes.pop()}


def transaction_ids(workload):
    ids = set()
    for packed in workload["transactions"]:
        raw = bytes.fromhex(packed.removeprefix("0x"))
        if not 0 < len(raw) <= 512 * 1024:
            raise ValueError("Transaction byte length is outside the sidechain limit")
        ids.add(hashlib.sha256(raw).hexdigest())
    if not 1 <= len(ids) <= 262_144 or len(ids) != len(workload["transactions"]):
        raise ValueError("Supply between 1 and 262,144 distinct signed transactions")
    return ids


def benchmark(args, network, nodes, workload, ids, chain, genesis_hash):
    stop = threading.Event()
    inclusions = [{} for _ in nodes]
    samples = []
    with futures.ThreadPoolExecutor(max_workers=len(nodes)) as pool:
        def sample(include_storage=False):
            return list(pool.map(lambda node: probe(node, chain, genesis_hash, include_storage), nodes))

        initial = sample()
        if len({tip["node_id"] for tip in initial}) != len(nodes) or any(tip["catching_up"] for tip in initial):
            raise RuntimeError("Use distinct CometBFT replicas that have finished catching up")
        before_block = common_block(nodes, initial)
        ingress = [node for node in nodes if node.submit]
        if args.prefund_payments:
            funding_order = [ingress[0], *(node for node in nodes if node is not ingress[0])]
            prefund(args, funding_order, workload["funding_transactions"])
        before = sample()
        begin = time.monotonic() + args.lead_seconds
        start_ns = time.time_ns() + round((begin - time.monotonic()) * 1e9)
        watchers = [pool.submit(observe, node, begin, stop, events, samples, tip["consensus_height"] + 1, ids)
                    for node, events, tip in zip(nodes, inclusions, before)]
        try:
            submitted = json.loads(run(args.loadgen, "-workload", args.workload,
                                       "-rpc-urls", ",".join(node.rpc for node in ingress),
                                       "-start-ns", str(start_ns), "-rate", str(args.rate),
                                       "-workers", str(args.client_workers), "-batch", str(args.rpc_batch),
                                       "-burst", str(args.burst)))
            for row in submitted:
                if "node" in row:
                    row["node"] = ingress[row["node"]].number
            # A transport error can race with successful admission on the replica.
            expected = {row["id"] for row in submitted if "id" in row and
                        (row.get("code") == 0 or "error" in row)}
            if args.contended:
                groups = [expected.intersection(row.get("id") for row in submitted[i:i + 4])
                          for i in range(0, len(submitted), 4)]
            else:
                groups = []
            while time.monotonic() < begin + args.seconds + args.drain and expected:
                for watcher in watchers:
                    if watcher.done():
                        watcher.result()
                if groups:
                    done = all(not group or all(group & node.keys() for node in inclusions) for group in groups)
                else:
                    done = all(expected <= node.keys() for node in inclusions)
                if done:
                    break
                time.sleep(0.05)
        finally:
            ended = time.monotonic()
            stop.set()
            for watcher in watchers:
                watcher.result()
        try:
            after = sample(include_storage=True)
        except (OSError, RuntimeError):
            summarize(args, submitted, inclusions, begin, samples, None, None, None, None, None, ended=ended)
            raise

    write_json(args.output / "remote-observations.json", {
        "network": network, "start_unix_ns": start_ns, "common_block_before": before_block,
        "before": before, "after": after,
    })
    summarize(args, submitted, inclusions, begin, samples, None, None, None, after, None, ended=ended)
    after_block = common_block(nodes, after)
    write_json(args.output / "common-block-after.json", after_block)

    visible = set.intersection(*(set(events) for events in inclusions))
    if not visible:
        raise RuntimeError("No workload transaction was observed on every replica")
    for tx in visible:
        if len({(events[tx]["height"], events[tx]["ckb_tx"], events[tx]["cycles"])
                for events in inclusions}) != 1:
            raise RuntimeError(f"Replicas disagree on execution receipt {tx}")
    observation = max((inclusions[0][tx] for tx in visible), key=lambda entry: (entry["height"], entry["ckb_tx"]))
    proof = args.output / "execution-proof.json"
    export_execution_proof(nodes[0], observation["height"], observation["ckb_tx"], proof, args.handoffs, args.genesis)
    result = run(args.proof_verifier, "-genesis", args.genesis, "-proof", proof)
    (args.output / "proof-verification.txt").write_text(result + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "genesis", "workload", "topology", "output", "loadgen", "proof-verifier"):
        parser.add_argument("--" + name, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--rate", type=float, required=True)
    parser.add_argument("--drain", type=float, default=30)
    parser.add_argument("--lead-seconds", type=float, default=3)
    parser.add_argument("--client-workers", type=int, default=24)
    parser.add_argument("--rpc-batch", type=int, default=8)
    parser.add_argument("--burst", type=int, default=0)
    parser.add_argument("--handoffs", type=lambda path: Path(path).resolve())
    parser.add_argument("--prefund-payments", action="store_true")
    parser.add_argument("--contended", action="store_true", help="expect one successful spend per group of four")
    args = parser.parse_args()
    if (not all(math.isfinite(value) and value > 0 for value in (args.rate, args.drain, args.lead_seconds)) or
            not 1 <= args.client_workers <= 512 or not 1 <= args.rpc_batch <= 10 or args.burst < 0):
        parser.error("Use positive finite durations and rate, 1–512 workers, and a batch size of 1–10")
    network = read_json(args.topology, 64 << 10)
    if network["placement"] not in ("same-host", "virtualized", "independent-hosts"):
        parser.error("Specify same-host, virtualized or independent-hosts placement")
    nodes = [Endpoint(args, i, entry) for i, entry in enumerate(network["replicas"])]
    if not 4 <= len(nodes) <= 32 or len({node.label for node in nodes}) != len(nodes) or not any(node.submit for node in nodes):
        parser.error("Specify 4–32 uniquely labelled replicas and at least one ingress")
    if network["placement"] == "independent-hosts" and len({node.host for node in nodes}) != len(nodes):
        parser.error("Record a distinct physical host for each independent-hosts replica")
    genesis = read_json(args.genesis, 24 << 20)
    workload = read_json(args.workload, MAX_WORKLOAD_BYTES)
    ids = transaction_ids(workload)
    if args.contended and len(ids) % 4:
        parser.error("The contended profile requires groups of four transactions")
    if args.prefund_payments and not workload.get("funding_transactions"):
        parser.error("The workload lacks its signed funding transactions")
    args.seconds = len(ids) / args.rate
    if not math.isfinite(args.seconds) or args.seconds >= 9e9:
        parser.error("The workload schedule exceeds the load generator's time range")
    args.output.mkdir(mode=0o700)
    app = args.output / "app-state.json"
    write_json(app, genesis["app_state"])
    genesis_hash = json.loads(run(args.binary, "genesis-hash", "--genesis", app))
    write_json(args.output / "run.json", {
        "args": {name: str(value) if isinstance(value, Path) else value for name, value in vars(args).items()},
        "commit": run("git", "rev-parse", "HEAD"), "source_dirty": bool(run("git", "status", "--porcelain")),
        "genesis_hash": genesis_hash, "genesis_sha256": sha256(args.genesis), "workload_sha256": sha256(args.workload),
        "binary_sha256": sha256(args.binary), "loadgen_sha256": sha256(args.loadgen),
        "proof_verifier_sha256": sha256(args.proof_verifier), "client_kernel": platform.release(),
    })
    benchmark(args, network, nodes, workload, ids, genesis["chain_id"], genesis_hash)


if __name__ == "__main__":
    main()

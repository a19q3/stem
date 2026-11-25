#!/usr/bin/env python3
"""Run signed CKB payments through a local CometBFT committee and record replica results."""

import argparse
import base64
from bisect import bisect_left
import concurrent.futures as futures
import datetime
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import re
import secrets
import select
import statistics
import subprocess
import threading
import time
import tomllib
import urllib.parse

from delay import NetworkDelay

CLOCK_TICKS = os.sysconf("SC_CLK_TCK")
PAGE_SIZE = os.sysconf("SC_PAGE_SIZE")
ROOT = Path(__file__).resolve().parents[1]
P2P_COUNTERS = ("cometbft_p2p_peer_send_bytes_total", "cometbft_p2p_peer_receive_bytes_total")
COMET_METRICS = P2P_COUNTERS + tuple(f"cometbft_mempool_{name}" for name in (
    "size", "size_bytes", "failed_txs", "rejected_txs", "evicted_txs", "recheck_times",
))


def run(*command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def write_json(path, data):
    path.write_text(json.dumps(data, separators=(",", ":")) + "\n")


def config_value(path, name, value):
    old = path.read_text()
    new, count = re.subn(rf"(?m)^{re.escape(name)} = .*?$", f"{name} = {value}", old)
    if not count:
        raise RuntimeError(f"Missing CometBFT config key {name}")
    path.write_text(new)


def process_resources(pid):
    proc = Path(f"/proc/{pid}")
    stat = (proc / "stat").read_text().rsplit(") ", 1)[1].split()
    io = dict(line.split(": ") for line in (proc / "io").read_text().splitlines())
    return {"cpu_s": (int(stat[11]) + int(stat[12])) / CLOCK_TICKS,
            "rss_bytes": int(stat[21]) * PAGE_SIZE, "process_written_bytes": int(io["write_bytes"])}


def allocated_storage(app_home, comet_data):
    used = {}
    for group, root in (("application", app_home), ("comet", comet_data)):
        for path in root.rglob("*"):
            try:
                if not path.is_file():
                    continue
                size = path.stat().st_blocks * 512
            except FileNotFoundError:
                continue
            prefix = path.relative_to(root).parts[0]
            if group == "application":
                if prefix == "snapshots":
                    category = "application_snapshots"
                else:
                    category = "application_wal" if re.fullmatch(r"\d+\.log", path.name) else "application_other"
            else:
                category = "consensus_wal" if prefix == "cs.wal" else prefix
            used[category] = used.get(category, 0) + size
    return used


def http_endpoint(address):
    if isinstance(address, int):
        address = f"http://127.0.0.1:{address}"
    url = urllib.parse.urlsplit(address)
    if (url.scheme not in ("http", "https") or not url.hostname or url.username is not None or
            "?" in address or "#" in address or "," in address):
        raise ValueError("Expected an HTTP(S) endpoint without credentials, query or fragment")
    cls = http.client.HTTPSConnection if url.scheme == "https" else http.client.HTTPConnection
    return cls, url.hostname, url.port, url.path


def prometheus_metrics(address, names, timeout=1):
    limit = 2 << 20
    connect, host, port, path = http_endpoint(address)
    connection = connect(host, port, timeout=timeout)
    try:
        connection.request("GET", path or "/metrics")
        reply = connection.getresponse()
        data = reply.read(limit + 1)
        if reply.status != 200 or len(data) > limit:
            raise OSError(f"Invalid Prometheus reply from {address}")
    finally:
        connection.close()
    result = {}
    for line in data.decode().splitlines():
        if not line or line.startswith("#"):
            continue
        name = re.split(r"[\s{]", line, maxsplit=1)[0]
        if name in names:
            value = float(line.rsplit(None, 1)[-1])
            result[name] = result.get(name, 0) + value
    return result


class Rpc:
    def __init__(self, address, timeout=5):
        connect, host, port, path = http_endpoint(address)
        self.connection = connect(host, port, timeout=timeout)
        self.path = path or "/"

    def call(self, method, **params):
        target = f"{self.path.rstrip('/')}/{method}?{urllib.parse.urlencode(params)}"
        attempts = 2 if method in ("status", "block", "block_results", "commit",
                                   "abci_info", "abci_query", "tx", "validators") else 1
        for attempt in range(attempts):
            try:
                self.connection.request("GET", target)
                response = self.connection.getresponse()
                document = self.read(response)
                break
            except (OSError, ValueError, http.client.HTTPException):
                self.connection.close()
                if attempt + 1 == attempts:
                    raise
        if "error" in document:
            raise RuntimeError(document["error"])
        return document["result"]

    def close(self):
        self.connection.close()

    @staticmethod
    def read(response):
        limit = 24 * 1024 * 1024
        data = response.read(limit + 1)
        if len(data) > limit:
            raise ValueError("CometBFT RPC reply exceeds the size limit")
        return json.loads(data)

    def broadcast_packed(self, packed):
        body = json.dumps({"id": 1, "jsonrpc": "2.0", "method": "broadcast_tx_sync", "params": {
            "tx": base64.b64encode(packed).decode("ascii"),
        }})
        self.connection.request("POST", self.path, body, {"Content-Type": "application/json"})
        response = self.connection.getresponse()
        document = self.read(response)
        if response.status != 200 or "error" in document:
            raise RuntimeError(f"Funding submission failed: {document}")
        return document["result"]


class Replica:
    def __init__(self, args, number):
        self.directory = args.output / f"node-{number}"
        self.directory.mkdir(mode=0o700)
        self.home = self.directory / "comet"
        self.app = self.directory / "app"
        self.rpc = args.base_port + number * 3
        self.p2p = self.rpc + 1
        self.dial = self.p2p + 120 if getattr(args, "p2p_delay_ms", 0) else self.p2p
        self.metrics = self.rpc + 2
        self.args = args
        self.binary = args.binary
        self.number = number
        self.vm_workers = args.vm_workers
        self.retain_consensus_blocks = getattr(args, "retain_consensus_blocks", 0)
        self.compact_transaction_history = False
        self.joined = []
        self.processes = []
        self.stopped_cpu = 0
        self.stopped_written = 0
        run(args.comet, "init", "--home", self.home)
        key = json.loads((self.home / "config" / "priv_validator_key.json").read_text())
        self.validator = {"address": key["address"], "pub_key": key["pub_key"],
                          "power": str(1 if number >= args.replicas - 2 else 2),
                          "name": f"validator-{number}"}
        self.address = run(args.comet, "show-node-id", "--home", self.home)
        self.config = self.home / "config" / "config.toml"
        config_value(self.config, "addr_book_strict", "false")
        config_value(self.config, "allow_duplicate_ip", "true")
        config_value(self.config, "prometheus", "true")
        config_value(self.config, "prometheus_listen_addr", f'"127.0.0.1:{self.metrics}"')
        config_value(self.config, "timeout_commit", f'"{args.commit_ms}ms"')
        config_value(self.config, "timeout_propose", '"1s"')
        config_value(self.config, "size", "4096")
        config_value(self.config, "max_txs_bytes", str(16 * 1024 * 1024))
        config_value(self.config, "max_tx_bytes", str(512 * 1024))
        if getattr(args, "no_comet_tx_index", False):
            config_value(self.config, "indexer", '"null"')

    def install(self, genesis, peers):
        self.peers = ",".join(f"{p.address}@127.0.0.1:{p.dial}" for p in peers if p is not self)
        write_json(self.home / "config" / "genesis.json", genesis)

    def start(self):
        retention = (["--retain-consensus-blocks", str(self.retain_consensus_blocks)]
                     if self.retain_consensus_blocks else [])
        index = ["--rpc-index"] if getattr(self, "rpc_index", False) else []
        history = ["--compact-transaction-history"] if self.compact_transaction_history else []
        admission = getattr(self.args, "admission_workers", None)
        admission_args = ["--admission-workers", str(admission)] if admission else []
        response_queue = getattr(self.args, "abci_response_queue", None)
        queue_args = ["--abci-response-queue", str(response_queue)] if response_queue is not None else []
        app = subprocess.Popen(
            [self.binary, "run", "--home", self.app, "--genesis",
             self.args.output / "app-state.json", "--workers", str(self.vm_workers),
             *admission_args, *queue_args, *retention, *index, *history],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        self.processes.append(app)
        # Do not treat a stale pathname left by SIGKILL as readiness.
        if not select.select([app.stdout], [], [], 20)[0]:
            raise TimeoutError(f"ABCI did not start for node {self.number}")
        line = app.stdout.readline()
        with (self.directory / "app.log").open("ab", buffering=0) as logs:
            logs.write(line)
        if b"event=abci_listening" not in line:
            raise RuntimeError(f"ABCI failed to start: {line.decode(errors='replace')}")
        thread = threading.Thread(target=self.copy_app_log, args=(app.stdout,), daemon=True)
        thread.start()
        self.joined.append(thread)
        with (self.directory / "comet.log").open("ab", buffering=0) as logs:
            comet = subprocess.Popen(
                [self.args.comet, "start", "--home", self.home, "--log_level", "info",
                 "--proxy_app", f"unix://{self.app / 'abci.sock'}",
                 "--rpc.laddr", f"tcp://127.0.0.1:{self.rpc}",
                 "--p2p.laddr", f"tcp://127.0.0.1:{self.p2p}",
                 "--p2p.persistent_peers", self.peers, "--p2p.pex=false",
                 "--consensus.create_empty_blocks=false",
                 "--consensus.create_empty_blocks_interval", self.args.idle_interval],
                stdout=logs, stderr=subprocess.STDOUT,
            )
            self.processes.append(comet)

    def copy_app_log(self, stream):
        with stream, (self.directory / "app.log").open("ab", buffering=0) as logs:
            while data := stream.readline():
                logs.write(data)

    def stop(self, crash=False):
        stats = self.resources()
        self.stopped_cpu = stats["cpu_s"]
        self.stopped_written = stats["process_written_bytes"]
        if crash:
            for process in reversed(self.processes):
                if process.poll() is None:
                    process.kill()
        for process in reversed(self.processes):
            if not crash and process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        self.processes.clear()
        for thread in self.joined:
            thread.join(timeout=1)
        self.joined.clear()

    def resources(self):
        cpu, rss, written = self.stopped_cpu, 0, self.stopped_written
        for process in tuple(self.processes):
            try:
                sample = process_resources(process.pid)
                cpu += sample["cpu_s"]
                rss += sample["rss_bytes"]
                written += sample["process_written_bytes"]
            except (FileNotFoundError, ProcessLookupError):
                pass
        return {"cpu_s": cpu, "rss_bytes": rss, "process_written_bytes": written}

    def telemetry(self):
        return prometheus_metrics(self.metrics, COMET_METRICS)

    def storage(self):
        return allocated_storage(self.app, self.home / "data")


def init_network(args, bridge=None, governance_lock=None, observers=0, guardian_governance_lock=None,
                 voting_powers=None):
    if voting_powers is not None and (len(voting_powers) != args.replicas or min(voting_powers) <= 0):
        raise ValueError("Supply one positive voting power per validator")
    nodes = [Replica(args, index) for index in range(args.replicas + observers)]
    if voting_powers is not None:
        for node, power in zip(nodes, voting_powers):
            node.validator["power"] = str(power)
    indexed_replica = getattr(args, "rpc_index_replica", None)
    if indexed_replica is not None:
        nodes[indexed_replica].rpc_index = True
    replicas = nodes[:args.replicas]
    genesis = json.loads((nodes[0].home / "config" / "genesis.json").read_text())
    genesis_date = datetime.datetime.now(datetime.UTC).replace(microsecond=0)
    genesis["genesis_time"] = genesis_date.isoformat().replace("+00:00", "Z")
    genesis["chain_id"] = f"fern-bench-{secrets.token_hex(4)}"
    genesis["validators"] = [node.validator for node in replicas]
    genesis["consensus_params"]["block"]["max_bytes"] = str(5 * 1024 * 1024)
    spec = {
        "chain_id": genesis["chain_id"],
        "timestamp_ms": int(genesis_date.timestamp()) * 1000,
        "epoch_length": 1000,
        "validators": [{"ed25519": "0x" + base64.b64decode(node.validator["pub_key"]["value"]).hex(),
                        "power": int(node.validator["power"])} for node in replicas],
        "allocations": [],
    }
    count = round(args.seconds * args.rate)
    if getattr(args, "checkpoint_interval", 0):
        spec["checkpoint_interval"] = args.checkpoint_interval
    if getattr(args, "checkpoint_cell_proofs", False):
        spec["checkpoint_cell_proofs"] = True
    if getattr(args, "checkpoint_compact_transactions", False):
        spec["checkpoint_compact_transactions"] = True
    if getattr(args, "checkpoint_live_cell_replay", False):
        spec["checkpoint_live_cell_replay"] = True
    if getattr(args, "compact_transaction_history", False):
        for node in nodes:
            node.compact_transaction_history = True
    if bridge is not None:
        spec["bridge"] = bridge
        spec["authenticate_withdrawals"] = not getattr(args, "legacy_withdrawal_receipts", False)
        count = 0
    write_json(args.output / "fixture-input.json", spec)
    extra = ["--governance-lock", governance_lock] if governance_lock else []
    if guardian_governance_lock is not None:
        extra.extend(("--guardian-governance-lock", guardian_governance_lock))
    if getattr(args, "workload", "payments") != "payments":
        extra.extend(["--workload", args.workload])
    if getattr(args, "xudt", None):
        extra.extend(["--xudt", args.xudt])
    if getattr(args, "prefund_payments", False):
        extra.append("--prefund-payments")
    fixture = json.loads(run(args.binary, "dev-fixture", "--spec",
                             args.output / "fixture-input.json", "--payments", str(count), *extra))
    genesis["app_state"] = fixture.pop("genesis")
    write_json(args.output / "app-state.json", genesis["app_state"])
    write_json(args.output / "workload.json", fixture)
    for node in nodes:
        node.install(genesis, nodes)
    return nodes, fixture


def await_genesis(nodes, timeout=35):
    deadline = time.monotonic() + timeout
    for node in nodes:
        rpc = Rpc(node.rpc, timeout=1)
        try:
            while time.monotonic() < deadline:
                for process in node.processes:
                    if process.poll() is not None:
                        raise RuntimeError(f"Node {node.number} exited. See {node.directory}")
                try:
                    status = rpc.call("status")["sync_info"]
                    if not status["catching_up"] and int(status["latest_block_height"]) >= 1:
                        break
                except (RuntimeError, OSError, http.client.HTTPException):
                    rpc.close()
                time.sleep(0.1)
            else:
                raise TimeoutError(f"Node {node.number} did not commit genesis. See {node.directory}")
        finally:
            rpc.close()


def wait_height(rpc, height):
    until = time.monotonic() + 25
    while time.monotonic() < until:
        if int(rpc.call("status")["sync_info"]["latest_block_height"]) >= height:
            return
        time.sleep(0.1)
    raise TimeoutError(f"Node did not reach height {height}")


def set_sync_config(replica, providers, trust_height, trust_hash):
    raw = replica.config.read_text()
    match = re.search(r"(?ms)^\[statesync\]\s*\n(.*?)(?=^\[|\Z)", raw)
    if not match:
        raise RuntimeError("State sync config section is unavailable")
    config = match[1]
    options = {
        "enable": "true",
        "rpc_servers": json.dumps(",".join(f"http://127.0.0.1:{p.rpc}" for p in providers)),
        "trust_height": str(trust_height),
        "trust_hash": json.dumps(trust_hash),
        "discovery_time": '"5s"',
    }
    for key, value in options.items():
        config, count = re.subn(rf"(?m)^{key} = .*?$", f"{key} = {value}", config)
        if count != 1:
            raise RuntimeError(f"Missing state sync config: {key}")
    replica.config.write_text(raw[:match.start(1)] + config + raw[match.end(1):])


def start_gateway(args, node):
    gateway = subprocess.Popen([
        args.binary, "rpc", "--genesis", args.output / "app-state.json",
        "--listen", f"127.0.0.1:{args.gateway_port}", "--comet-rpc", f"http://127.0.0.1:{node.rpc}/",
    ], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        if not select.select([gateway.stdout], [], [], 12)[0]:
            raise RuntimeError("CKB RPC gateway did not start")
        ready = gateway.stdout.readline()
        if b"event=ckb_rpc_listening" not in ready:
            raise RuntimeError(ready.decode(errors="replace"))
        return gateway
    except BaseException:
        gateway.kill()
        gateway.communicate()
        raise


def stop_gateway(gateway):
    gateway.terminate()
    try:
        gateway.communicate(timeout=8)
    except subprocess.TimeoutExpired:
        gateway.kill()
        gateway.communicate()


def block_execution(rpc, height):
    txs = rpc.call("block", height=height)["block"]["data"].get("txs") or []
    if not txs:
        return []
    results = rpc.call("block_results", height=height)["txs_results"]
    if len(txs) != len(results):
        raise ValueError("CometBFT transaction and execution result counts differ")
    return [(base64.b64decode(raw), result) for raw, result in zip(txs, results)]


def observe(node, begin, stop, inclusions, samples, first_height=1, transaction_ids=None):
    rpc = Rpc(node.rpc, timeout=1)
    next_height = first_height
    last_sample = 0
    try:
        while not stop.is_set():
            now = time.monotonic()
            if now - last_sample >= 1:
                sample = {"node": node.number, "time_s": now - begin, **node.resources()}
                try:
                    sample.update(node.telemetry())
                except (OSError, http.client.HTTPException):
                    pass
                samples.append(sample)
                last_sample = now
            try:
                status = rpc.call("status")["sync_info"]
                if next_height < int(status["earliest_block_height"]):
                    raise ValueError(f"Replica {node.number} pruned unobserved height {next_height}")
                for height in range(next_height, int(status["latest_block_height"]) + 1):
                    execution = block_execution(rpc, height)
                    seen = time.monotonic() - begin
                    for raw, result in execution:
                        if result.get("code", 0) == 0:
                            tx = hashlib.sha256(raw).hexdigest()
                            if transaction_ids is None or tx in transaction_ids:
                                ckb_tx = base64.b64decode(result["data"])[:32].hex()
                                inclusions[tx] = {"seen_s": seen, "height": height, "ckb_tx": ckb_tx,
                                                  "cycles": int(result["gas_used"])}
                    next_height = height + 1
            except (RuntimeError, OSError, http.client.HTTPException):
                rpc.close()
            stop.wait(0.05)
    finally:
        rpc.close()


def fault(node, seconds, outage, begin):
    time.sleep(max(0, begin + seconds - time.monotonic()))
    started = time.monotonic() - begin
    node.stop(crash=True)
    time.sleep(outage)
    node.start()
    return {"node": node.number, "crash_s": started, "restarted_s": time.monotonic() - begin}


def load(args, nodes, fixture, begin):
    if args.loadgen:
        endpoints = nodes[:-1] if args.outage_seconds else nodes
        start_ns = time.time_ns() + round((begin - time.monotonic()) * 1_000_000_000)
        return json.loads(run(
            args.loadgen, "-workload", args.output / "workload.json",
            "-ports", ",".join(str(node.rpc) for node in endpoints),
            "-start-ns", str(start_ns), "-rate", str(args.rate),
            "-workers", str(args.client_workers), "-batch", str(args.rpc_batch), "-burst", str(args.burst),
        ))
    transactions = fixture["transactions"]
    local = threading.local()

    def submit(index, raw):
        pool_size = len(nodes) - bool(args.outage_seconds)
        node = nodes[index % pool_size]
        connections = getattr(local, "connections", None)
        if connections is None:
            connections = local.connections = {}
        rpc = connections.setdefault(node.rpc, Rpc(node.rpc))
        started = time.monotonic() - begin
        tx = hashlib.sha256(bytes.fromhex(raw[2:])).hexdigest()
        result = {"id": tx, "node": node.number, "scheduled_s": index / args.rate, "offered_s": started}
        try:
            check = rpc.call("broadcast_tx_sync", tx=raw)
            result["code"] = check.get("code", 0)
            if result["code"]:
                result["log"] = check.get("log")
        except (RuntimeError, OSError, http.client.HTTPException) as error:
            rpc.close()
            result["error"] = str(error)
        result["ack_s"] = time.monotonic() - begin
        return result

    with futures.ThreadPoolExecutor(max_workers=args.client_workers) as executor:
        submitted = []
        outstanding = set()
        for index, raw in enumerate(transactions):
            target = begin + index / args.rate
            time.sleep(max(0, target - time.monotonic()))
            outstanding = {job for job in outstanding if not job.done()}
            if len(outstanding) >= args.client_workers * 4:
                # A saturated load generator is a measurement limitation. Record
                # drops instead of claiming that its internal queue offered this TPS.
                result = futures.Future()
                result.set_result({"scheduled_s": index / args.rate, "client_dropped": True})
            else:
                result = executor.submit(submit, index, raw)
                outstanding.add(result)
            submitted.append(result)
        return [job.result() for job in submitted]


def prefund(args, nodes, splits):
    peers = [Rpc(node.rpc, timeout=15) for node in nodes]
    committed = []
    started = time.monotonic()
    try:
        first_height = int(peers[0].call("status")["sync_info"]["latest_block_height"]) + 1
        for start in range(0, len(splits), 16):
            pending = set()
            for raw in splits[start:start + 16]:
                packed = bytes.fromhex(raw[2:])
                txid = hashlib.sha256(packed).hexdigest()
                check = peers[0].broadcast_packed(packed)
                if check["code"] or check["hash"].lower() != txid:
                    raise RuntimeError(f"Funding split failed admission: {check}")
                pending.add(txid)
            deadline = time.monotonic() + 45
            confirmations = []
            for rpc in peers:
                found = {}
                next_height = first_height
                while len(found) < len(pending) and time.monotonic() < deadline:
                    head = int(rpc.call("status")["sync_info"]["latest_block_height"])
                    for height in range(next_height, head + 1):
                        for raw, result in block_execution(rpc, height):
                            txid = hashlib.sha256(raw).hexdigest()
                            if txid in pending:
                                if result["code"] != 0:
                                    raise RuntimeError(f"Funding execution failed: {result}")
                                found[txid] = (height, base64.b64decode(result["data"]).hex())
                    next_height = max(next_height, head + 1)
                    if len(found) < len(pending):
                        time.sleep(0.1)
                if len(found) < len(pending):
                    raise TimeoutError(f"Funding splits did not reach every replica: {pending - found.keys()}")
                confirmations.append(found)
            if any(found != confirmations[0] for found in confirmations):
                raise RuntimeError("Replicas disagree on funding receipts")
            for txid, (height, ckb_tx) in confirmations[0].items():
                committed.append({"hash": txid, "ckb_tx": ckb_tx, "height": height})
            first_height = max(height for height, _ in confirmations[0].values()) + 1
    finally:
        for rpc in peers:
            rpc.close()
    last_height = max(row["height"] for row in committed)
    write_json(args.output / "prefunding.json", {"height": last_height, "seconds": time.monotonic() - started,
                                              "replicas": len(peers), "funding_transactions": committed})
    return last_height + 1


def tip(node):
    rpc = Rpc(node.rpc)
    try:
        info = rpc.call("abci_info")["response"]
        latest = rpc.call("status")["sync_info"]
        return {"application_height": int(info["last_block_height"]),
                "application_hash": info["last_block_app_hash"],
                "consensus_height": int(latest["latest_block_height"]),
                "consensus_base": int(latest["earliest_block_height"]),
                "block_hash": latest["latest_block_hash"]}
    finally:
        rpc.close()


def idle_phase(nodes, seconds, directory):
    before = [(tip(node), node.resources()) for node in nodes]
    time.sleep(seconds)
    after = [(tip(node), node.resources()) for node in nodes]
    write_json(directory / "idle.json", {
        "seconds": seconds,
        "nodes": [{
            "blocks": end_tip["application_height"] - start_tip["application_height"],
            "app_hash_equal": end_tip["application_hash"] == start_tip["application_hash"],
            "cpu_s": end["cpu_s"] - start["cpu_s"],
            "process_written_bytes": end["process_written_bytes"] - start["process_written_bytes"],
            "allocated_bytes": node.storage(),
        } for node, (start_tip, start), (end_tip, end) in zip(nodes, before, after)]
    })


def wait_proof_height(rpc, height):
    deadline = time.monotonic() + 15
    while int(rpc.call("status")["sync_info"]["latest_block_height"]) <= height:
        if time.monotonic() >= deadline:
            raise TimeoutError("Missing successor commit for the execution receipt")
        time.sleep(0.1)


def export_execution_proof(node, height, tx_hash, path, handoffs=None, genesis=None):
    rpc = Rpc(node.rpc)
    try:
        wait_proof_height(rpc, height)
    finally:
        rpc.close()
    genesis = genesis or node.home / "config" / "genesis.json"
    endpoint = f"http://127.0.0.1:{node.rpc}" if isinstance(node.rpc, int) else node.rpc
    command = [node.args.proof_verifier, "-genesis", genesis,
               "-rpc", endpoint, "-height", str(height), "-ckb-tx", tx_hash]
    if handoffs:
        command.extend(("-handoffs", handoffs))
    path.write_text(run(*command) + "\n")
    return command


def execution_proof(node, inclusions, directory):
    tx_id, observation = max(inclusions.items(), key=lambda item: item[1]["height"])
    height = observation["height"]
    path = directory / "execution-proof.json"
    if getattr(node.args, "no_comet_tx_index", False):
        export_execution_proof(node, height, observation["ckb_tx"], path)
        return
    rpc = Rpc(node.rpc)
    try:
        wait_proof_height(rpc, height)
        write_json(path, {
            "commit": rpc.call("commit", height=height),
            "next_commit": rpc.call("commit", height=height + 1),
            "tx": rpc.call("tx", hash="0x" + tx_id, prove="true"),
            "results": rpc.call("block_results", height=height),
        })
    finally:
        rpc.close()


def quantiles(values):
    if not values:
        return None
    ordered = sorted(values)
    return {f"p{p}": round(ordered[max(0, (len(values) * p + 99) // 100 - 1)] * 1000, 3)
            for p in (50, 95, 99)}


def summarize(args, submitted, inclusions, begin, samples, before, after, storage, tips, fault_result, ended=None):
    offered = [row for row in submitted if "offered_s" in row]
    admitted = [row for row in offered if row.get("code") == 0]
    visible = [row for row in offered if all(row["id"] in node for node in inclusions)]
    finalized = [row for row in offered if row["id"] in inclusions[0]]
    visible_times = {row["id"]: max(node[row["id"]]["seen_s"] for node in inclusions) for row in visible}
    latencies = [inclusions[0][row["id"]]["seen_s"] - row["offered_s"] for row in finalized]
    elapsed = (time.monotonic() if ended is None else ended) - begin
    times = {
        "offered": sorted(row["offered_s"] for row in offered),
        "admitted": sorted(row["ack_s"] for row in admitted),
        "finalized_node_0": sorted(row["seen_s"] for row in inclusions[0].values()),
        "visible_all": sorted(visible_times.values()),
    }
    interval = [
        {"second": second, **{key: bisect_left(values, second) for key, values in times.items()}}
        for second in range(1, int(elapsed) + 1)
    ]
    def resource_delta(key):
        if before is not None and after is not None and all(key in sample for sample in (*before, *after)):
            return sum(a[key] - b[key] for a, b in zip(after, before))
        return None

    summary = {
        "scheduled": len(submitted), "offered": len(offered), "admitted": len(admitted),
        "finalized_node_0": len(finalized), "visible_all": len(visible),
        "visible_without_admission_ack": sum(row.get("code") != 0 for row in visible),
        "client_dropped": len(submitted) - len(offered),
        "load_seconds": args.seconds, "elapsed_s": elapsed, "duration_after_offered_s": elapsed - args.seconds,
        "offered_tps": sum(row["offered_s"] < args.seconds for row in offered) / args.seconds,
        "admitted_tps": sum(row["ack_s"] < args.seconds for row in admitted) / args.seconds,
        "finalized_tps": sum(inclusions[0][row["id"]]["seen_s"] < args.seconds for row in finalized) / args.seconds,
        "all_replica_tps": sum(seen < args.seconds for seen in visible_times.values()) / args.seconds,
        "completion": len(visible) / len(offered) if offered else 0,
        "finality_latency_ms": quantiles(latencies),
        "all_replica_latency_ms": quantiles([visible_times[row["id"]] - row["offered_s"] for row in visible]),
        "median_payment_cycles": statistics.median(row["cycles"] for row in inclusions[0].values()) if finalized else None,
        "cpu_s": resource_delta("cpu_s"),
        "process_written_bytes": resource_delta("process_written_bytes"),
        "storage_allocated_bytes_by_node": storage, "tips": tips, "fault": fault_result,
        "cumulative_per_second": interval,
    }
    write_json(args.output / "admission.json", submitted)
    write_json(args.output / "inclusion-by-node.json", inclusions)
    write_json(args.output / "resources.json", samples)
    write_json(args.output / "summary.json", summary)
    print(json.dumps(summary, indent=2))


def parse():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--comet", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--replicas", type=int, default=4)
    parser.add_argument("--base-port", type=int, default=27100)
    parser.add_argument("--rate", type=float, default=32)
    parser.add_argument("--seconds", type=float, default=10)
    parser.add_argument("--drain", type=float, default=15)
    parser.add_argument("--commit-ms", type=int, default=200)
    parser.add_argument("--idle-interval", default="10s")
    parser.add_argument("--idle-seconds", type=float, default=0)
    parser.add_argument("--vm-workers", type=int, default=2)
    parser.add_argument("--admission-workers", type=int, help="separate workers for mempool checks")
    parser.add_argument("--abci-response-queue", type=int, help="queued responses per ABCI connection")
    parser.add_argument("--client-workers", type=int, default=24)
    parser.add_argument("--loadgen", type=Path, help="use the Go JSON-RPC load generator")
    parser.add_argument("--rpc-batch", type=int, default=8)
    parser.add_argument("--burst", type=int, default=0, help="schedule this many transactions at each burst")
    parser.add_argument("--p2p-delay-ms", type=float, default=0, help="fixed one-way loopback TCP delay")
    parser.add_argument("--rpc-index-replica", type=int, help="enable the wallet index on this zero-based replica")
    parser.add_argument("--checkpoint-interval", type=int, default=0)
    parser.add_argument("--checkpoint-cell-proofs", action="store_true")
    parser.add_argument("--checkpoint-compact-transactions", action="store_true")
    parser.add_argument("--checkpoint-live-cell-replay", action="store_true")
    parser.add_argument("--compact-transaction-history", action="store_true")
    parser.add_argument("--retain-consensus-blocks", type=int, default=0)
    parser.add_argument("--no-comet-tx-index", action="store_true", help="disable CometBFT's optional transaction index")
    parser.add_argument("--sync-observer", action="store_true", help="restore a fresh replica after measuring load")
    parser.add_argument("--sync-checkpoint-height", type=int, help="wait for and restore this checkpoint height")
    parser.add_argument("--serial-replay", action="store_true", help="compare a fresh single-worker replay from genesis")
    parser.add_argument("--outage-at", type=float, default=2)
    parser.add_argument("--outage-seconds", type=float, default=0)
    parser.add_argument("--proof-verifier", type=Path)
    parser.add_argument("--workload", choices=("payments", "xudt", "mixed", "atomic", "contended"),
                        default="payments")
    parser.add_argument("--xudt", type=Path, help="RISC-V xUDT ELF for typed workloads")
    parser.add_argument("--prefund-payments", action="store_true", help="split development CKB on chain before sampling")
    args = parser.parse_args()
    for name in ("binary", "comet", "output"):
        setattr(args, name, getattr(args, name).resolve())
    if args.proof_verifier:
        args.proof_verifier = args.proof_verifier.resolve()
    if args.loadgen:
        args.loadgen = args.loadgen.resolve()
    if (args.workload in ("xudt", "mixed", "atomic")) != bool(args.xudt):
        parser.error("Typed workloads require --xudt and --workload")
    if args.xudt:
        args.xudt = args.xudt.resolve()
    if args.burst < 0 or (args.burst and not args.loadgen) or args.p2p_delay_ms < 0:
        parser.error("Use nonnegative burst and delay, and the Go load generator for bursts")
    if not 1 <= args.rpc_batch <= 10:
        parser.error("CometBFT's local RPC config accepts batches of up to 10 requests")
    maximum = 262_144 if args.prefund_payments else 65_536
    if not 1 <= round(args.seconds * args.rate) <= maximum:
        parser.error(f"Run must schedule between 1 and {maximum:,} transactions")
    if args.prefund_payments and (args.workload != "payments" or not args.loadgen):
        parser.error("Prefunding requires the Go load generator and the CKB payments workload")
    if not 4 <= args.replicas <= 32:
        parser.error("Use between 4 and 32 replicas")
    if args.admission_workers is not None and not 1 <= args.admission_workers <= 64:
        parser.error("Admission workers must be between 1 and 64")
    if args.abci_response_queue is not None and not 1 <= args.abci_response_queue <= 128:
        parser.error("ABCI response queue must be between 1 and 128")
    if args.rpc_index_replica is not None and not 0 <= args.rpc_index_replica < args.replicas:
        parser.error("RPC index replica is outside the local committee")
    if args.checkpoint_interval and args.checkpoint_interval < 4:
        parser.error("Use a checkpoint interval of at least four blocks")
    if args.checkpoint_cell_proofs and not args.checkpoint_interval:
        parser.error("Live Cell proofs require periodic checkpoints")
    if args.checkpoint_compact_transactions and not args.checkpoint_cell_proofs:
        parser.error("Compact checkpoints require Live Cell proofs")
    if args.checkpoint_live_cell_replay and not args.checkpoint_compact_transactions:
        parser.error("Live Cell replay protection requires compact checkpoints")
    if args.compact_transaction_history and not args.checkpoint_compact_transactions:
        parser.error("Compact transaction history requires compact checkpoints")
    if args.sync_observer and not args.checkpoint_cell_proofs:
        parser.error("The state-sync observer requires proof-enabled checkpoints")
    if args.no_comet_tx_index and not args.proof_verifier:
        parser.error("Exporting a proof without the CometBFT index requires --proof-verifier")
    if args.retain_consensus_blocks < 0 or (args.retain_consensus_blocks and not args.checkpoint_interval):
        parser.error("Pruning requires a positive retained block count and periodic checkpoints")
    if args.sync_checkpoint_height is not None and (not args.sync_observer or
            args.sync_checkpoint_height <= 0 or args.sync_checkpoint_height % args.checkpoint_interval):
        parser.error("The state-sync target must be a positive checkpoint height")
    return args


def measure_sync(args, validators, observer, payments):
    replicas = [Rpc(replica.rpc, timeout=15) for replica in (validators[0], observer)]
    origin, joining = replicas
    directory = validators[0].app / "snapshots"
    try:
        checkpoint = args.sync_checkpoint_height
        if checkpoint is None:
            heights = [int(path.stem) for path in directory.glob("*.meta")]
            if not heights:
                raise RuntimeError("Load did not produce a checkpoint")
            checkpoint = max(heights)
        until = time.monotonic() + 45
        while time.monotonic() < until:
            if (directory / f"{checkpoint}.meta").is_file() and all(
                tip(validator)["application_height"] >= checkpoint + 2 for validator in validators[:2]
            ):
                break
            time.sleep(0.2)
        else:
            raise TimeoutError(f"Checkpoint {checkpoint} didn't obtain its two successors")
        trust = origin.call("block", height=checkpoint)["block_id"]["hash"]
        set_sync_config(observer, validators[:2], checkpoint, trust)
        started = time.monotonic()
        observer.start()
        await_genesis([observer])
        elapsed = time.monotonic() - started
        match = re.search(r"event=snapshot_installed height=(\d+)", (observer.directory / "app.log").read_text())
        if not match:
            raise RuntimeError("The state-sync observer replayed from genesis")
        restored = int(match[1])
        if restored != checkpoint:
            raise RuntimeError(f"Observer restored height {restored}, expected {checkpoint}")
        image = observer.app / "snapshots" / f"{restored}.bin"
        phase = re.findall(r"(?m)^I\[([^\]]+)\]\s+(?:Snapshot accepted, restoring|Snapshot restored)\s+"
                           rf"module=statesync height={restored}\b", (observer.directory / "comet.log").read_text())
        if len(phase) != 2:
            raise RuntimeError("Observer did not log an unambiguous snapshot installation phase")
        accepted, installed = (datetime.datetime.fromisoformat(stamp.replace("|", "T")) for stamp in phase)
        replay_target = restored + 2
        deadline = time.monotonic() + 15
        while int(joining.call("abci_info")["response"]["last_block_height"]) < replay_target:
            if time.monotonic() >= deadline:
                raise TimeoutError(f"Observer did not apply through height {replay_target}")
            time.sleep(0.1)
        replay_seconds = time.monotonic() - started
        base = int(origin.call("status")["sync_info"]["earliest_block_height"])
        txid, entry = max((item for item in payments.items() if base <= item[1]["height"] < restored),
                          key=lambda item: item[1]["height"], default=(None, None))
        if entry is None:
            raise RuntimeError("The restored checkpoint contains no measured payment in the provider's retained blocks")
        tx_hash = entry["ckb_tx"]
        route = "/rpc/transaction" if args.checkpoint_compact_transactions else "/transaction"
        response = joining.call("abci_query", path=json.dumps(route), data="0x" + tx_hash)["response"]
        if response["code"] != 0:
            raise RuntimeError(f"Restored observer could not find the payment: {response}")
        record = base64.b64decode(response["value"])
        packed = next(raw for raw, _ in block_execution(origin, entry["height"])
                      if hashlib.sha256(raw).hexdigest() == txid)
        if len(record) < 32 or record[32:] != (b"" if args.checkpoint_compact_transactions else packed):
            raise RuntimeError("Snapshot restored unexpected transaction data")
        header_hash = "0x" + record[:32].hex()
        headers = [rpc.call("abci_query", path='"/header"', data=header_hash)["response"] for rpc in replicas]
        if any(header["code"] != 0 for header in headers) or headers[0]["value"] != headers[1]["value"]:
            raise RuntimeError("Snapshot omitted the transaction's origin header")
        report = {"trusted_height": checkpoint, "snapshot_height": restored, "sync_seconds": elapsed,
                  "application_replay_target": replay_target, "application_replay_seconds": replay_seconds,
                  "transfer_and_install_seconds": (installed - accepted).total_seconds(),
                  "image_bytes": image.stat().st_size, "sample_ckb_tx": "0x" + tx_hash,
                  "sample_header_hash": header_hash, "sample_transaction_body_retained": len(record) > 32,
                  "observer_resources": observer.resources(),
                  "observer_p2p": prometheus_metrics(observer.metrics, P2P_COUNTERS), "tip": tip(observer)}
        write_json(args.output / "loaded-snapshot-sync.json", report)
        print(json.dumps(report, indent=2))
    finally:
        for rpc in replicas:
            rpc.close()


def measure_serial_replay(args, validators, observer):
    tips = [tip(node) for node in validators]
    if any(tip["consensus_base"] != 1 for tip in tips):
        raise RuntimeError("Serial replay from genesis requires retained blocks from height one")
    height = min(min(tip["application_height"], tip["consensus_height"]) for tip in tips)
    observer.vm_workers = 1
    observer.retain_consensus_blocks = 0
    started = time.monotonic()
    observer.start()
    await_genesis([observer], timeout=120)
    observer_rpc = Rpc(observer.rpc)
    try:
        # CometBFT can switch out of block sync before the target height.
        wait_height(observer_rpc, height)
    finally:
        observer_rpc.close()
    elapsed = time.monotonic() - started
    replica_tip = tip(observer)
    if replica_tip["application_height"] < height or replica_tip["consensus_base"] != 1:
        raise RuntimeError("The serial replica did not replay the chain from genesis")

    nodes = [*validators, observer]
    connections = [Rpc(node.rpc, timeout=15) for node in nodes]
    blocks = []
    try:
        for number in range(1, height + 1):
            results = [rpc.call("block_results", height=number) for rpc in connections]
            if any(result != results[0] for result in results[1:]):
                raise RuntimeError(f"Replicas disagree on ABCI execution results at height {number}")
            block_ids = [rpc.call("block", height=number)["block_id"]["hash"] for rpc in connections]
            if any(block != block_ids[0] for block in block_ids[1:]):
                raise RuntimeError(f"Replicas disagree on the block at height {number}")
            result = results[0]
            blocks.append({"height": number, "block_hash": block_ids[0], "application_hash": result["app_hash"],
                           "transactions": len(result["txs_results"] or []),
                           "results_sha256": hashlib.sha256(json.dumps(result, sort_keys=True).encode()).hexdigest()})
        metrics = json.loads(base64.b64decode(
            connections[-1].call("abci_query", path='"/metrics"')["response"]["value"]))
    finally:
        for rpc in connections:
            rpc.close()

    report = {"height": height, "replay_seconds": elapsed, "observer_tip": replica_tip, "observer_metrics": metrics,
              "vm_workers": [node.vm_workers for node in nodes], "blocks": blocks}
    checkpoint = height - height % args.checkpoint_interval if args.checkpoint_interval else 0
    if checkpoint:
        images = []
        for node in nodes:
            path = node.app / "snapshots" / str(checkpoint)
            with path.with_suffix(".bin").open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            images.append({"sha256": digest, "metadata": path.with_suffix(".meta").read_bytes().hex()})
        if any(image != images[0] for image in images[1:]):
            raise RuntimeError(f"Replicas disagree on the checkpoint at height {checkpoint}")
        report["checkpoint"] = {"height": checkpoint, **images[0]}
    write_json(args.output / "serial-replay.json", report)


def measure(args, nodes, fixture, observer=None, replay=None):
    stop = threading.Event()
    inclusions = [{} for _ in nodes]
    samples = []
    try:
        for node in nodes:
            node.start()
        await_genesis(nodes)
        first_height = prefund(args, nodes, fixture.pop("funding_transactions")) if args.prefund_payments else 1
        if args.idle_seconds > 0:
            idle_phase(nodes, args.idle_seconds, args.output)
        before = [node.resources() for node in nodes]
        lead_time = 2.0 if args.prefund_payments else (0.6 if args.loadgen else 0.15)
        begin = time.monotonic() + lead_time
        with futures.ThreadPoolExecutor(max_workers=len(nodes) + 1) as executor:
            watchers = [executor.submit(observe, node, begin, stop, events, samples, first_height)
                        for node, events in zip(nodes, inclusions)]
            outage = (executor.submit(fault, nodes[-1], args.outage_at, args.outage_seconds, begin)
                      if args.outage_seconds else None)
            try:
                submitted = load(args, nodes, fixture, begin)
                ended = begin + args.seconds + args.drain
                expected = {row["id"] for row in submitted if row.get("code") == 0}
                if args.workload == "contended":
                    groups = [{row["id"] for row in submitted[i:i + 4] if row.get("code") == 0}
                              for i in range(0, len(submitted), 4)]
                    completed = lambda: all(not group or all(group & node.keys() for node in inclusions)
                                            for group in groups)
                else:
                    completed = lambda: all(expected <= node.keys() for node in inclusions)
                while time.monotonic() < ended and not completed():
                    time.sleep(0.05)
                if not len(expected):
                    write_json(args.output / "admission.json", submitted)
                    raise RuntimeError("No payment was admitted. See replica logs.")
                fault_result = outage.result() if outage else None
            finally:
                stop.set()
                for watcher in watchers:
                    watcher.result()
        after = [node.resources() for node in nodes]
        storage = [node.storage() for node in nodes]
        if inclusions[0]:
            execution_proof(nodes[0], inclusions[0], args.output)
            if args.proof_verifier:
                proof_check = run(args.proof_verifier, "-proof", args.output / "execution-proof.json",
                                  "-genesis", nodes[0].home / "config" / "genesis.json")
                (args.output / "proof-verification.txt").write_text(proof_check + "\n")
        tips = [tip(node) for node in nodes]
        for node in nodes:
            rpc = Rpc(node.rpc)
            try:
                for path, name in (("/metrics", "app-metrics.json"), ("/storage_stats", "app-storage.json")):
                    query = rpc.call("abci_query", path=json.dumps(path))["response"]
                    if query["code"] == 0:
                        write_json(node.directory / name, json.loads(base64.b64decode(query["value"])))
            finally:
                rpc.close()
        summarize(args, submitted, inclusions, begin, samples, before, after, storage, tips, fault_result)
        if observer:
            measure_sync(args, nodes, observer, inclusions[0])
        if replay:
            measure_serial_replay(args, nodes, replay)
    finally:
        if replay:
            replay.stop()
        if observer:
            observer.stop()
        for node in reversed(nodes):
            node.stop()


def main():
    args = parse()
    args.output.mkdir(mode=0o700)
    params = vars(args).copy()
    with (ROOT / "Cargo.toml").open("rb") as manifest:
        ckb_base = tomllib.load(manifest)["workspace"]["dependencies"]["ckb-types"]["rev"]
    params.update({"ckb_base": ckb_base,
                   "commit": run("git", "rev-parse", "HEAD"),
                   "source_dirty": bool(run("git", "status", "--porcelain")),
                   "binary_sha256": hashlib.file_digest(args.binary.open("rb"), "sha256").hexdigest(),
                   "comet_version": run(args.comet, "version"), "rustc": run("rustc", "-V"),
                   "kernel": platform.release(), "host_cpus": os.cpu_count()})
    if args.xudt:
        with args.xudt.open("rb") as program:
            params["xudt_sha256"] = hashlib.file_digest(program, "sha256").hexdigest()
    (args.output / "run.json").write_text(json.dumps(params, default=str, indent=2) + "\n")
    nodes, fixture = init_network(args, observers=int(args.sync_observer) + int(args.serial_replay))
    if args.loadgen:
        fixture.pop("transactions")
    routes = [(node.dial, node.p2p) for node in nodes if node.dial != node.p2p]
    observers = iter(nodes[args.replicas:])
    observer = next(observers) if args.sync_observer else None
    replay = next(observers) if args.serial_replay else None
    with NetworkDelay(routes, args.p2p_delay_ms):
        measure(args, nodes[:args.replicas], fixture, observer, replay)


if __name__ == "__main__":
    main()

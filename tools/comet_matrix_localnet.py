#!/usr/bin/env python3
"""Run the frozen CometBFT baseline with the Jolteon matrix's payment fixture."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

from jolteon_localnet import evidence_keys, genesis_and_payments
from localnet import Rpc, config_value, process_resources, run, write_json
from native_bridge_localnet import wait_for


def start(args, command, log):
    with (args.output / log).open("wb", buffering=0) as file:
        return subprocess.Popen(command, stdout=file, stderr=subprocess.STDOUT,
                                env={**os.environ, "GOMAXPROCS": "2"})


def run_scenario(args):
    keys = evidence_keys(args)
    weights = [1 + i % 2 for i in range(args.validators)]
    spec, payments = genesis_and_payments(args, keys, weights)
    run(args.comet, "testnet", "--v", str(args.validators), "--n", "0", "--o", args.output / "net")
    homes = [args.output / "net" / f"node{i}" for i in range(args.validators)]
    ids = [run(args.comet, "show-node-id", "--home", home) for home in homes]
    genesis = json.loads((homes[0] / "config/genesis.json").read_text())
    native_id = json.loads(run(args.binary, "genesis-hash", "--genesis", spec))
    genesis["chain_id"] = "stem-jolteon-devnet"
    genesis["genesis_time"] = "2025-01-01T00:00:00Z"
    genesis["app_state"] = {"stem_genesis": native_id.removeprefix("0x")}
    genesis["validators"] = []
    for i, home in enumerate(homes):
        public, path = keys[i]
        address = hashlib.sha256(public).digest()[:20].hex().upper()
        pub_key = {"type": "tendermint/PubKeyEd25519", "value": base64.b64encode(public).decode()}
        priv_key = {"type": "tendermint/PrivKeyEd25519",
                    "value": base64.b64encode(path.read_bytes() + public).decode()}
        genesis["validators"].append({"name": f"node{i}", "power": str(weights[i]), **dict(address=address, pub_key=pub_key)})
        write_json(home / "config/priv_validator_key.json", {"address": address, "pub_key": pub_key, "priv_key": priv_key})
    p2p = [args.port + args.validators + i for i in range(args.validators)]
    for i, home in enumerate(homes):
        write_json(home / "config/genesis.json", genesis)
        config = home / "config/config.toml"
        peers = ",".join(f"{peer}@127.0.0.1:{p2p[j]}" for j, peer in enumerate(ids) if i != j)
        values = {
            "timeout_commit": f'"{args.cadence}ms"', "timeout_propose": f'"{args.timeout}ms"',
            "timeout_prevote": f'"{args.timeout}ms"', "timeout_precommit": f'"{args.timeout}ms"',
            "skip_timeout_commit": "false", "allow_duplicate_ip": "true", "pex": "false",
            "max_num_inbound_peers": args.validators, "max_num_outbound_peers": args.validators,
            "persistent_peers": f'"{peers}"',
        }
        for name, value in values.items():
            config_value(config, name, str(value))

    children = []
    clients = [Rpc(args.port + i) for i in range(args.validators)]

    def status():
        for child in children:
            if child.poll() is not None:
                raise AssertionError(f"baseline process exited with code {child.poll()}")
        return [int(rpc.call("status")["sync_info"]["latest_block_height"]) for rpc in clients]

    def net_stats():
        values = []
        for rpc in clients:
            peers = rpc.call("net_info")["peers"] or []
            sent = sum(int(p["connection_status"]["SendMonitor"]["Bytes"]) for p in peers)
            received = sum(int(p["connection_status"]["RecvMonitor"]["Bytes"]) for p in peers)
            values.append({"peers": len(peers), "sent_bytes": sent, "received_bytes": received})
        return values

    try:
        apps = []
        for i, home in enumerate(homes):
            app = args.output / f"app{i}"
            apps.append(app)
            children.append(start(args, [args.binary, "run", "--home", app, "--genesis", spec, "--archive"], f"app{i}.log"))
        wait_for(lambda: all((app / "abci.sock").is_socket() for app in apps), "baseline ABCI sockets", seconds=60)
        for i, home in enumerate(homes):
            socket = apps[i] / "abci.sock"
            children.append(start(args, [args.comet, "start", "--home", home,
                                         "--proxy_app", f"unix://{socket}", "--rpc.laddr", f"tcp://127.0.0.1:{args.port+i}",
                                         "--p2p.laddr", f"tcp://127.0.0.1:{p2p[i]}"], f"comet{i}.log"))
        def all_ready():
            values = status()
            return values if min(values) >= 3 else None
        heights = wait_for(all_ready, "every baseline replica finalized", seconds=90)
        before_net = net_stats()
        before_proc = [process_resources(child.pid) for child in children]
        started = time.perf_counter()
        ids_ = []
        for index, tx in enumerate(payments):
            result = clients[index % len(clients)].call("broadcast_tx_sync", tx=tx)
            if result["code"] != 0:
                raise AssertionError(result)
            ids_.append(result["hash"])
            time.sleep(max(0, started + len(ids_) / args.offered - time.perf_counter()))

        def payments_committed():
            results = [clients[0].call("tx", hash="0x" + id_.removeprefix("0x")) for id_ in ids_]
            if any(tx["tx_result"]["code"] != 0 for tx in results):
                raise AssertionError("baseline included a rejected transaction")
            height = max(int(tx["height"]) for tx in results)
            return height if min(status()) >= max(height, min(heights) + 4) else None

        final_height = wait_for(payments_committed, "all baseline replicas see the workload", seconds=90)
        elapsed = time.perf_counter() - started
        views = [rpc.call("abci_query", path=json.dumps("/stem/header"), height=final_height)["response"] for rpc in clients]
        if any(view["code"] != 0 or view["value"] != views[0]["value"] for view in views):
            raise AssertionError("baseline replicas disagree on the execution Header")
        report = {
            "validators": args.validators, "weights": weights, "payments": len(ids_), "confirm_seconds": elapsed,
            "confirmed_tps": len(ids_) / elapsed, "finalized_height": final_height,
            "execution_header": views[0]["value"], "before": before_net, "after": net_stats(),
            "processes_before": before_proc, "processes": [process_resources(child.pid) for child in children],
        }
        write_json(args.output / "comet-report.json", report)
        return report
    finally:
        diagnostics = []
        for i, child in enumerate(children):
            entry = {"exit": child.poll()}
            if i >= len(clients) and entry["exit"] is None:
                try:
                    entry["height"] = int(clients[i - len(clients)].call("status")["sync_info"]["latest_block_height"])
                except Exception as exc:
                    entry["rpc_error"] = str(exc)
            diagnostics.append(entry)
        write_json(args.output / "last-status.json", diagnostics)
        for rpc in clients:
            rpc.close()
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
        stop = time.monotonic() + 8
        for child in children:
            try:
                child.wait(timeout=max(0, stop - time.monotonic()))
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "lock", "output"):
        parser.add_argument("--" + flag, type=lambda text: Path(text).resolve(), required=True)
    parser.add_argument("--validators", type=int, default=4)
    parser.add_argument("--transactions", type=int, default=32)
    parser.add_argument("--port", type=int, default=26000)
    parser.add_argument("--cadence", type=int, default=300)
    parser.add_argument("--timeout", type=int, default=2500)
    parser.add_argument("--offered", type=float, default=16)
    args = parser.parse_args()
    args.output.mkdir(mode=0o700, parents=True)
    result = run_scenario(args)
    print(f"{result['validators']} baseline validators finalized {result['payments']} payments")


if __name__ == "__main__":
    main()

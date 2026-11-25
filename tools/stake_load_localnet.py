"""Load a stake-backed Stem network using only confirmed CKB bridge funding."""

import base64
import concurrent.futures
import hashlib
import json
import math
import subprocess
import tempfile
import time
from pathlib import Path

from bridge_localnet import CKB, FEE, ckb_hash, dependency, hex_bytes, point, script
from localnet import Rpc, process_resources, run, write_json
from native_bridge_localnet import cell, deposit_and_mint, signing_cell, stem_id

COUNT = 384
RATE = 8


def prepare(args, l1, fixture, vault, spec, spec_path, lane, client):
    mint = deposit_and_mint(args, l1, fixture, vault, spec, spec_path, lane, client)
    code_hash = hex_bytes(ckb_hash(args.native_lock.read_bytes()))
    lock = script(code_hash, fixture.key(fixture.owner)["blake160"], "data")
    output = 3 * CKB
    change = mint["payment_capacity"] - COUNT * output - FEE
    if change <= output:
        raise AssertionError("bridge-funded benchmark has insufficient Capacity")
    with tempfile.TemporaryDirectory(dir=args.output) as directory:
        temporary = Path(directory)
        lock_path = temporary / "lock.json"
        write_json(lock_path, lock)
        deps = [dependency(mint["lock_dep"])]
        code = signing_cell(client, mint["lock_dep"])

        def sign(index, input_cell, outputs):
            request = temporary / f"{index}.json"
            draft = temporary / f"{index}-draft.json"
            context = temporary / f"{index}-context.json"
            write_json(request, {"inputs": [{"out_point": input_cell["out_point"]}],
                                 "outputs": outputs, "cell_deps": deps})
            write_json(context, {"cells": [input_cell, code]})
            write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request)))
            return json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
                                  "--transaction", draft, "--context", context,
                                  "--group-index", "0", "--lock", lock_path,
                                  "--key-file", fixture.owner))

        split = sign("split", signing_cell(client, mint["payment"]),
                     [cell(output, lock)] * COUNT + [cell(change, lock)])
        accepted = client.call("broadcast_tx_commit", tx=split)
        if accepted["check_tx"]["code"] != 0 or accepted.get("tx_result", {}).get("code") != 0:
            raise AssertionError(accepted)
        parent = stem_id(args, spec["network_id"], split)

        def payment(index):
            return sign(index, {"out_point": point(parent, index), "cell": cell(output, lock)},
                        [cell(output - FEE, lock)])

        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            transactions = list(pool.map(payment, range(COUNT)))
    workload = args.output / "stake-payment-workload.json"
    write_json(workload, {"transactions": transactions})
    write_json(args.output / "stake-load-ckb-funding.json", {"mint": mint, "split_height": int(accepted["height"])})
    return workload


def sample(nodes, clients):
    system = [process_resources(node.pid) for node in nodes]
    sent = received = 0
    for client in clients:
        for peer in client.call("net_info")["peers"]:
            connection = peer["connection_status"]
            sent += int(connection["SendMonitor"]["Bytes"])
            received += int(connection["RecvMonitor"]["Bytes"])
    return (
        sum(item["cpu_s"] for item in system),
        sum(item["rss_bytes"] for item in system),
        sum(item["process_written_bytes"] for item in system),
        sent, received,
    )


def measure(args, workload, nodes):
    clients = [Rpc(args.comet_port + index) for index in range(2)]
    events = args.output / "stake-load-client.json"
    visibility = {}
    activity = []
    start = time.time_ns() + 500_000_000
    initial = sample(nodes, clients)
    peak = initial[1]
    try:
        cursor = min(int(rpc.call("status")["sync_info"]["latest_block_height"]) for rpc in clients)
        with events.open("wb") as records, (args.output / "stake-load-client.log").open("wb") as log:
            sender = subprocess.Popen(
                [args.loadgen, "-workload", workload, "-ports", f"{args.comet_port},{args.comet_port + 1}",
                 "-rate", str(RATE), "-batch", "4", "-workers", "8", "-start-ns", str(start)],
                stdout=records, stderr=log,
            )
            deadline = time.monotonic() + COUNT / RATE + 25
            rows = None
            try:
                while time.monotonic() < deadline:
                    limit = min(int(rpc.call("status")["sync_info"]["latest_block_height"]) for rpc in clients)
                    while cursor < limit:
                        height = cursor + 1
                        try:
                            execution = clients[0].call("block_results", height=height)
                        except RuntimeError as exc:
                            error = exc.args[0] if exc.args else None
                            pending = (isinstance(error, dict)
                                       and error.get("data") == f"could not find results for height #{height}")
                            if pending:
                                # The block store can publish a height before the state store has its results.
                                break
                            raise
                        observed = (time.time_ns() - start) / 1e9
                        data = clients[0].call("block", height=height)["block"]["data"]["txs"] or []
                        receipts = execution["txs_results"] or []
                        cursor = height
                        if len(receipts) != len(data):
                            raise AssertionError("incomplete CometBFT execution results")
                        for encoded, result in zip(data, receipts):
                            if result["code"] == 0:
                                tx_id = hashlib.sha256(base64.b64decode(encoded)).hexdigest()
                                visibility[tx_id] = observed
                        if execution.get("validator_updates"):
                            activity.append({"height": cursor, "visible_s": observed, "updates": execution["validator_updates"]})
                    peak = max(peak, sum(process_resources(node.pid)["rss_bytes"] for node in nodes))
                    if rows is None and sender.poll() is not None:
                        if sender.returncode:
                            raise RuntimeError("stake load client failed")
                        rows = json.loads(events.read_text())
                    if rows and all(item["id"].lower() in visibility for item in rows if item.get("code") == 0):
                        break
                    time.sleep(0.08)
            finally:
                if sender.poll() is None:
                    sender.terminate()
                sender.wait(timeout=5)
        if rows is None:
            raise RuntimeError("stake load client did not finish")
        final = sample(nodes, clients)
    finally:
        for client in clients:
            client.close()
    submitted = [row for row in rows if "offered_s" in row]
    admitted = [row for row in submitted if row.get("code") == 0]
    admitted_ids = {row["id"].lower() for row in admitted}
    first_seen = {key: value for key, value in visibility.items() if key in admitted_ids}
    latency = sorted(max(0, first_seen[row["id"].lower()] - row["offered_s"]) * 1000
                     for row in admitted if row["id"].lower() in first_seen)
    if not latency:
        raise AssertionError("stake load did not confirm any payment")
    offertime = max(COUNT / RATE, max(row["offered_s"] for row in submitted))
    report = {
        "offered": len(submitted), "admitted": len(admitted), "finalized_both_replicas": len(latency),
        "offered_tps": len(submitted) / offertime, "admitted_tps": len(admitted) / offertime,
        "finalized_tps": len(latency) / max(first_seen.values()),
        "replica_visible_ms": {f"p{p}": latency[math.ceil(len(latency) * p / 100) - 1] for p in (50, 95, 99)},
        "client_drops": sum(row.get("client_dropped", False) for row in rows),
        "cpu_seconds": final[0] - initial[0], "peak_sampled_rss_bytes": peak,
        "process_write_bytes": final[2] - initial[2],
        "p2p_sent_bytes": final[3] - initial[3], "p2p_received_bytes": final[4] - initial[4],
        "validator_updates": activity,
    }
    if not {9, 19}.issubset({item["height"] for item in activity}):
        raise AssertionError(f"payments did not span both validator updates: {report}")
    write_json(args.output / "stake-load-first-seen.json", first_seen)
    write_json(args.output / "stake-load-report.json", report)
    return report

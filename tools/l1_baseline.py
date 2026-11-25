#!/usr/bin/env python3
"""Measure the original CKB dev node with signed one-input payments and paced blocks."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import threading
import time

from bridge_localnet import (
    CKB, FAUCET_ARG, FAUCET_KEY, FEE, SIGHASH_CODE, CkbRpc, L1,
    dependency, output, point, script, transaction, wallet_group, write_key,
)
from localnet import CLOCK_TICKS, PAGE_SIZE, quantiles, run, write_json


def prepare(args, node):
    genesis = node.call("get_block_by_number", "0x0")
    faucet_tx, dep_groups = genesis["transactions"][:2]
    index, funds = next((i, cell) for i, cell in enumerate(faucet_tx["outputs"])
                        if cell["lock"]["args"] == FAUCET_ARG)
    count = round(args.seconds * args.rate)
    lock = script(SIGHASH_CODE, FAUCET_ARG)
    faucet = write_key(args.output, "faucet", FAUCET_KEY)
    recipient_key = write_key(args.output, "recipient", bytes([2] * 32))
    recipient = json.loads(run(args.binary, "wallet", "key", "--key-file", recipient_key))
    recipient_path = args.output / "recipient.json"
    write_json(recipient_path, script(SIGHASH_CODE, recipient["blake160"]))
    one = 100 * CKB
    changes = int(funds["capacity"], 16) - count * one - FEE
    build = transaction([point(faucet_tx["hash"], index)], [output(one, lock)] * count + [output(changes, lock)],
                        ["0x"] * (count + 1), [dependency(point(dep_groups["hash"], 0), "dep_group")])
    tx_file, groups = args.output / "funding.json", args.output / "sign-groups.json"
    write_json(tx_file, build)
    write_json(groups, [wallet_group(0, [faucet])])
    signed = json.loads(run(args.binary, "wallet", "sign", "--transaction", tx_file, "--groups", groups))
    funding = node.send(signed)
    txs = json.loads(run(args.binary, "wallet", "payments", "--funding-hash", funding,
                         "--dep-group-hash", dep_groups["hash"], "--recipient", recipient_path,
                         "--key-file", faucet, "--payments", str(count)))
    write_json(args.output / "transactions.json", txs)
    return txs


def mine(args, begin, stop, records):
    rpc = CkbRpc(args.l1_port)
    try:
        sequence = 0
        while not stop.is_set():
            target = begin + sequence * args.block_ms / 1000
            if stop.wait(max(0, target - time.monotonic())):
                break
            started = time.monotonic()
            block = rpc.call("generate_block")
            records.append({"hash": block, "start_s": started - begin,
                            "produced_s": time.monotonic() - begin})
            sequence += 1
    finally:
        rpc.close()


def resource(pid):
    proc = Path(f"/proc/{pid}")
    stat = (proc / "stat").read_text().split(") ", 1)[1].split()
    io = dict(line.split(": ") for line in (proc / "io").read_text().splitlines())
    return {"cpu_s": (int(stat[11]) + int(stat[12])) / CLOCK_TICKS, "rss_bytes": int(stat[21]) * PAGE_SIZE,
            "process_written_bytes": int(io["write_bytes"])}


def observe(args, begin, stop, height, txs, final, included, samples, pid):
    rpc = CkbRpc(args.l1_port)
    pending_confirmations = {}
    last_sample = 0
    try:
        while not stop.is_set():
            now = time.monotonic()
            if now - last_sample >= 1:
                samples.append({"time_s": now - begin, **resource(pid)})
                last_sample = now
            tip = int(rpc.call("get_tip_block_number"), 16)
            for number in range(height + 1, tip + 1):
                block = rpc.call("get_block_by_number", hex(number))
                now = time.monotonic() - begin
                for tx in block["transactions"]:
                    if tx["hash"] in txs:
                        included[tx["hash"]] = {"seen_s": now, "block": number}
                        pending_confirmations.setdefault(number + args.depth, []).append(tx["hash"])
                for tx in pending_confirmations.pop(number, []):
                    final[tx] = now
                height = number
            stop.wait(0.05)
    finally:
        rpc.close()


def submit(args, begin, txs):
    local = threading.local()

    def request(entry, scheduled):
        if getattr(local, "rpc", None) is None:
            local.rpc = CkbRpc(args.l1_port)
        started = time.monotonic() - begin
        row = {"hash": entry["hash"], "scheduled_s": scheduled, "offered_s": started}
        try:
            ack = local.rpc.call("send_transaction", entry["tx"])
            row["admitted"] = ack == entry["hash"]
        except (OSError, RuntimeError) as error:
            local.rpc.close()
            row["error"] = str(error)
        row["ack_s"] = time.monotonic() - begin
        return row

    calls, dropped, alive = [], 0, set()
    with ThreadPoolExecutor(max_workers=args.clients) as executor:
        for index, tx in enumerate(txs):
            scheduled = index / args.rate
            time.sleep(max(0, begin + scheduled - time.monotonic()))
            alive = {job for job in alive if not job.done()}
            if len(alive) >= args.clients * 4:
                dropped += 1
                continue
            job = executor.submit(request, tx, scheduled)
            alive.add(job)
            calls.append(job)
        return [job.result() for job in calls], dropped


def baseline(args, node):
    txs = prepare(args, node)
    start_height = int(node.call("get_tip_block_number"), 16)
    pid = node.process.pid
    first = resource(pid)
    begin = time.monotonic() + 0.2
    stop = threading.Event()
    blocks, samples, final, included = [], [], {}, {}
    ids = {row["hash"] for row in txs}
    with ThreadPoolExecutor(max_workers=2) as pool:
        miner = pool.submit(mine, args, begin, stop, blocks)
        observer = pool.submit(observe, args, begin, stop, start_height, ids, final, included, samples, pid)
        try:
            offered, client_drops = submit(args, begin, txs)
            accepted = {row["hash"] for row in offered if row.get("admitted")}
            deadline = begin + args.seconds + 20
            while time.monotonic() < deadline and not accepted <= final.keys():
                if miner.done():
                    miner.result()
                if observer.done():
                    observer.result()
                time.sleep(0.1)
        finally:
            stop.set()
            miner.result()
            observer.result()
    last = resource(pid)
    result = {
        "l1_version": run(args.l1, "--version"),
        "l1_sha256": hashlib.file_digest(args.l1.open("rb"), "sha256").hexdigest(),
        "genesis": node.call("get_block_hash", "0x0"),
        "scheduled": len(txs), "offered": len(offered), "admitted": len(accepted), "included": len(included),
        "depth_confirmed": len(final), "client_drops": client_drops, "seconds": args.seconds,
        "block_interval_ms": args.block_ms, "confirmation_depth": args.depth,
        "offered_tps": sum(row["offered_s"] < args.seconds for row in offered) / args.seconds,
        "admitted_tps": sum(row.get("admitted", False) and row["ack_s"] < args.seconds for row in offered) / args.seconds,
        "included_tps": sum(row["seen_s"] < args.seconds for row in included.values()) / args.seconds,
        "depth_confirmed_tps": sum(t < args.seconds for t in final.values()) / args.seconds,
        "inclusion_ms": quantiles([included[row["hash"]]["seen_s"] - row["offered_s"]
                                    for row in offered if row["hash"] in included]),
        "depth_confirmed_ms": quantiles([final[row["hash"]] - row["offered_s"]
                                        for row in offered if row["hash"] in final]),
        "cpu_s": last["cpu_s"] - first["cpu_s"], "peak_rss_bytes": max(r["rss_bytes"] for r in samples),
        "process_written_bytes": last["process_written_bytes"] - first["process_written_bytes"],
    }
    for name, data in (("baseline.json", result), ("offers.json", offered), ("blocks.json", blocks),
                       ("resources.json", samples), ("included.json", included), ("depth.json", final)):
        write_json(args.output / name, data)
    print(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "l1", "output"):
        parser.add_argument("--" + flag, required=True, type=Path)
    parser.add_argument("--rate", type=float, default=256)
    parser.add_argument("--seconds", type=float, default=18)
    parser.add_argument("--depth", type=int, default=6)
    parser.add_argument("--block-ms", type=int, default=500)
    parser.add_argument("--clients", type=int, default=24)
    parser.add_argument("--l1-port", type=int, default=28500)
    args = parser.parse_args()
    if not 1 <= round(args.rate * args.seconds) <= 6000 or args.depth < 1 or args.block_ms < 100:
        parser.error("Use 1–6000 payments, positive confirmation depth, and at least 100 ms per block")
    for name in ("binary", "l1", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir(mode=0o700)
    node = L1(args)
    try:
        baseline(args, node)
    finally:
        node.stop()


if __name__ == "__main__":
    main()

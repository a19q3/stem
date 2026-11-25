#!/usr/bin/env python3
"""Relay the original signed boundary and successor of a funded Jolteon handoff."""

import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import time
from types import SimpleNamespace

from bridge_localnet import CkbRpc, L1, point
from jolteon_checkpoint_contention import connect, default_pool_policies, import_setup_blocks
from jolteon_ckb_relay_localnet import relay_checkpoint
from localnet import process_resources, write_json
from native_bridge_localnet import start, wait_for


def open_source(args):
    home = args.output / "source-l1"
    shutil.copytree(args.stem_run / "l1", home, ignore=shutil.ignore_patterns("l1.log"))
    path = home / "ckb.toml"
    config, rpc = re.subn(r'(?m)^listen_address = "127\.0\.0\.1:[0-9]+"$',
                         f'listen_address = "127.0.0.1:{args.source_port}"', path.read_text())
    config, p2p = re.subn(r'(?m)^listen_addresses = \["/ip4/0\.0\.0\.0/tcp/[0-9]+"\]$',
                         f'listen_addresses = ["/ip4/127.0.0.1/tcp/{args.source_port + 1}"]', config)
    assert rpc == p2p == 1, "expected a stock fixture CKB configuration"
    path.write_text(config)
    node = CkbRpc(args.source_port)
    process = start(args, [args.l1, "run", "-C", home], "source-l1.log")
    def ready():
        assert process.poll() is None, "copied CKB source process exited"
        try:
            return node.call("get_tip_block_number")
        except (OSError, RuntimeError):
            node.close()
            raise

    try:
        wait_for(ready, "saved CKB test chain", seconds=30)
    except BaseException:
        stop_source(node, process)
        raise
    return node, process


def stop_source(node, process):
    node.close()
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def verify_source(args, source):
    report = json.loads((args.stem_run / "wide-handoff-report.json").read_text())
    publications = report["checkpoint"]["publications"]
    assert len(publications) == 3 and publications[1]["height"] == report["boundary"]["height"], publications
    signed_transactions = []
    for publication in publications:
        height, tx_hash = publication["height"], publication["transaction"]
        inclusion = publication["l1_inclusion"]
        header, proof = inclusion["header"], inclusion["proof"]
        assert source.call("get_block_hash", header["number"]) == header["hash"]
        assert source.call("verify_transaction_and_witness_proof", proof) == [tx_hash]
        signed = json.loads((args.stem_run / f"jolteon-checkpoints-checkpoint-{height}-signed.json").read_text())
        assert signed["hash"] == tx_hash
        saved = source.call("get_transaction", tx_hash, "0x0", True)["transaction"]
        assert saved == signed["molecule"], (height, tx_hash)
        signed_transactions.append(signed)
    for previous, following in zip(signed_transactions, signed_transactions[1:]):
        assert following["json"]["inputs"][0]["previous_output"] == point(previous["hash"], 0)
    return report, signed_transactions


def run_scenario(args, source, origin, peer):
    report, signed = verify_source(args, source)
    publications = report["checkpoint"]["publications"]
    prefix = publications[0]["l1_inclusion"]["header"]
    setup_tip = int(prefix["number"], 16)
    budgets = [int(node.call("get_consensus")["max_block_cycles"], 16)
               for node in (source, origin, peer)]
    assert budgets == [args.ckb_block_cycles] * 3, budgets
    ckb_genesis = source.call("get_block_hash", "0x0")
    assert all(node.call("get_block_hash", "0x0") == ckb_genesis for node in (origin, peer))
    assert not any(node.call("get_peers") for node in (source, origin, peer)), "expected three disconnected CKB nodes"
    policy = default_pool_policies(origin, peer)
    started = time.perf_counter()
    assert import_setup_blocks(source, origin, peer, through=setup_tip) == setup_tip
    setup_import_seconds = time.perf_counter() - started
    assert all(node.call("get_tip_header")["hash"] == prefix["hash"] for node in (origin, peer))
    assert connect(origin, peer, args.relay_port) == prefix["hash"]

    skipped = json.loads((args.stem_run / "ckb-reject-skipped-wide-handoff-signed.json").read_text())
    rejection = origin.reject(skipped)
    assert "error code 43" in rejection.lower(), rejection
    write_json(args.output / "invalid-successor.json", {"signed": skipped, "rejection": rejection})

    connected = {"origin": origin, "peer": peer}
    observed = []
    for publication, tx in zip(publications[1:], signed[1:]):
        tx_hash = tx["hash"]
        predecessor = tx["json"]["inputs"][0]["previous_output"]
        for node in connected.values():
            # A fresh node has never imported this transaction or its proposal.
            entry = node.call("get_transaction", tx_hash)
            assert not entry or entry["tx_status"]["status"] == "unknown", entry
            assert node.call("get_live_cell", predecessor, True)["status"] == "live"
        cycles = int(origin.call("estimate_cycles", tx["json"])["cycles"], 16)
        assert cycles == publication["estimated_cycles"], (cycles, publication["estimated_cycles"])
        if report["validators"] == 128:
            assert cycles > policy[0]["max_tx_verify_cycles"], cycles
        before = {name: process_resources(node.process.pid) for name, node in connected.items()}
        received = relay_checkpoint(origin, peer, tx)
        after = {name: process_resources(node.process.pid) for name, node in connected.items()}
        assert received["peer_inclusion"]["header"]["hash"] != publication["l1_inclusion"]["header"]["hash"]
        committed_cycles = {}
        for name, node in connected.items():
            committed = node.call("get_transaction", tx_hash, "0x0", True)
            assert committed["transaction"] == tx["molecule"], tx_hash
            details = node.call("get_transaction", tx_hash, "0x1", True)
            committed_cycles[name] = int(details["cycles"], 16)
            assert committed_cycles[name] == cycles, details
            cell = node.call("get_live_cell", point(tx_hash, 0), True)
            assert cell["status"] == "live" and cell["cell"]["data"]["content"] == tx["json"]["outputs_data"][0]
            assert node.call("get_live_cell", predecessor, False)["status"] != "live"
        observed.append({"stem_height": publication["height"], "transaction": tx_hash,
                         "source_inclusion": publication["l1_inclusion"], "witness_bytes": publication["witness_bytes"],
                         "estimated_cycles": cycles, "committed_cycles": committed_cycles,
                         "before_resources": before, "after_resources": after, **received})
        write_json(args.output / "relayed-checkpoints.json", observed)

    assert source.call("get_block_hash", prefix["number"]) == prefix["hash"]
    assert not source.call("get_peers"), "the copied source must stay disconnected"
    network = {name: node.call("get_peers") for name, node in connected.items()}
    assert len(network["origin"]) == len(network["peer"]) == 1, network
    assert network["origin"][0]["node_id"] == peer.call("local_node_info")["node_id"]
    assert network["peer"][0]["node_id"] == origin.call("local_node_info")["node_id"]
    results = {"validators": report["validators"], "boundary": report["boundary"],
               "original_checkpoints": publications, "ckb_genesis": ckb_genesis, "setup_header": prefix,
               "setup_import_seconds": setup_import_seconds, "pool_policies": policy,
               "ckb_block_cycles": budgets[0], "ckb_peers": network,
               "skipped_handoff_rejected": rejection, "relayed": observed}
    write_json(args.output / "epoch-relay-report.json", results)
    print(f"Relayed the {report['validators']}-member boundary and successor via CKB P2P")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("l1", "stem-run", "output"):
        parser.add_argument(f"--{flag}", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--source-port", type=int, default=30200)
    parser.add_argument("--l1-port", type=int, default=30210)
    parser.add_argument("--relay-port", type=int, default=30220)
    parser.add_argument("--ckb-block-cycles", type=int, default=3_500_000_000)
    args = parser.parse_args()
    endpoints = (args.source_port, args.l1_port, args.relay_port)
    ports = {port + offset for port in endpoints for offset in (0, 1)}
    if len(ports) != 6 or not all(1024 <= port <= 65535 for port in ports) or args.ckb_block_cycles <= 0:
        parser.error("provide three disjoint CKB RPC and P2P port pairs and a positive block budget")
    args.output.mkdir(parents=True, mode=0o700)
    peer_output = args.output / "receiver"
    peer_output.mkdir(mode=0o700)
    source, process = open_source(args)
    try:
        origin = L1(args)
        try:
            peer = L1(SimpleNamespace(l1=args.l1, l1_port=args.relay_port, output=peer_output,
                                     ckb_block_cycles=args.ckb_block_cycles))
            try:
                run_scenario(args, source, origin, peer)
            finally:
                peer.stop()
        finally:
            origin.stop()
    finally:
        stop_source(source, process)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Verify strict weighted quorum and recovery after a local P2P partition."""

import argparse
import hashlib
import json
from pathlib import Path
import time

from bridge_localnet import sidechain_tx
from delay import NetworkDelay
from localnet import Rpc, await_genesis, execution_proof, init_network, run, write_json


def routes_for(nodes, first_port):
    routes, crossing = [], []
    for src in nodes:
        peers = []
        for dst in nodes:
            if src is dst:
                continue
            port = first_port + len(routes)
            routes.append((port, dst.p2p))
            if (src.number < 2) != (dst.number < 2):
                crossing.append(port)
            peers.append(f"{dst.address}@127.0.0.1:{port}")
        src.peers = ",".join(peers)
    return routes, crossing


def peer_sets(rpcs):
    return [{peer["node_info"]["id"] for peer in rpc.call("net_info")["peers"]} for rpc in rpcs]


def wait_peer_sets(rpcs, expected, seconds=30):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        peers = peer_sets(rpcs)
        if peers == expected:
            return
        time.sleep(0.15)
    raise TimeoutError(f"Peers did not reach the expected topology: {peers}")


def heights(rpcs):
    return [int(rpc.call("status")["sync_info"]["latest_block_height"]) for rpc in rpcs]


def submit(rpc, raw):
    packed = bytes.fromhex(raw[2:])
    txid = hashlib.sha256(packed).hexdigest()
    check = rpc.broadcast_packed(packed)
    if check["code"] or check["hash"].lower() != txid:
        raise RuntimeError(f"Partitioned replica did not admit the valid payment: {check}")
    return txid


def receipts(rpcs, txid):
    deadline = time.monotonic() + 40
    results = {}
    while len(results) < len(rpcs) and time.monotonic() < deadline:
        for number, rpc in enumerate(rpcs):
            if number in results:
                continue
            try:
                tx = rpc.call("tx", hash="0x" + txid)
            except RuntimeError as error:
                if "not found" in str(error):
                    continue
                raise
            results[number] = (int(tx["height"]), tx["tx_result"])
        if len(results) < len(rpcs):
            time.sleep(0.1)
    if len(results) < len(rpcs):
        raise TimeoutError(f"Payment {txid} was absent after the partition healed: {results}")
    height, result = results[0]
    if result["code"] or any(value != (height, result) for value in results.values()):
        raise RuntimeError(f"Replicas disagree on the recovered receipt: {results}")
    return height


def scenario(args):
    nodes, fixture = init_network(args)
    routes, crossing = routes_for(nodes, args.route_port)
    all_ids = {node.address for node in nodes}
    full_mesh = [all_ids - {node.address} for node in nodes]
    isolated = [{nodes[number ^ 1].address} for number in range(len(nodes))]
    rpcs = [Rpc(node.rpc) for node in nodes]
    with NetworkDelay(routes, args.delay_ms) as network:
        try:
            for node in nodes:
                node.start()
            await_genesis(nodes)
            wait_peer_sets(rpcs, full_mesh)
            initial = sidechain_tx(nodes[0], fixture["transactions"][0])
            receipts(rpcs, initial["hash"].lower())

            network.block(crossing)
            wait_peer_sets(rpcs, isolated)
            # Allow in-flight messages from before the cut to drain.
            time.sleep(0.6)
            before = heights(rpcs)
            left = submit(rpcs[0], fixture["transactions"][1])
            right = submit(rpcs[2], fixture["transactions"][2])
            blocked = time.monotonic()
            observed = []
            while time.monotonic() < blocked + args.hold_seconds:
                sample = heights(rpcs)
                if sample != before or peer_sets(rpcs) != isolated:
                    raise RuntimeError(f"Partition changed during the quorum check: {before} -> {sample}")
                observed.append(sample)
                time.sleep(0.2)

            released = time.monotonic()
            network.block(())
            confirmed = {txid: receipts(rpcs, txid) for txid in (left, right)}
            recovered = time.monotonic() - released
            wait_peer_sets(rpcs, full_mesh, seconds=45)
            proofs = {}
            genesis = nodes[0].home / "config" / "genesis.json"
            for side, txid in (("two_thirds", left), ("one_third", right)):
                directory = args.output / side
                directory.mkdir()
                execution_proof(nodes[0], {txid: {"height": confirmed[txid]}}, directory)
                proofs[side] = run(args.proof_verifier, "-genesis", genesis,
                                   "-proof", directory / "execution-proof.json")
            result = {
                "replicas": len(nodes), "delay_one_way_ms": args.delay_ms,
                "island_voting_power": [sum(int(node.validator["power"]) for node in nodes[:2]),
                                        sum(int(node.validator["power"]) for node in nodes[2:])],
                "partition_seconds": released - blocked, "partitions": [sorted(group) for group in isolated],
                "heights_during_partition": observed, "confirmed_after_heal": confirmed,
                "heal_to_all_replica_confirmation_seconds": recovered, "proof_verification": proofs,
            }
            write_json(args.output / "partition.json", result)
            print(json.dumps(result, indent=2))
        finally:
            for rpc in rpcs:
                rpc.close()
            for node in reversed(nodes):
                node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "comet", "output", "proof-verifier"):
        parser.add_argument("--" + name, required=True, type=lambda value: Path(value).resolve())
    parser.add_argument("--base-port", type=int, default=33100)
    parser.add_argument("--route-port", type=int, default=33200)
    parser.add_argument("--delay-ms", type=float, default=20)
    parser.add_argument("--hold-seconds", type=float, default=6)
    args = parser.parse_args()
    if args.hold_seconds <= 0 or args.delay_ms < 0:
        parser.error("Use a positive hold time and nonnegative delay")
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "2s", 3, 1
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

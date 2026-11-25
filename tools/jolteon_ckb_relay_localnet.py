#!/usr/bin/env python3
"""Submit a real multi-voter checkpoint to one default CKB node and mine it on a peer."""

import argparse
import json
from pathlib import Path
import time
import tomllib
from types import SimpleNamespace

from bridge_localnet import CKB, FAUCET_ARG, FEE, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes, le, output, point, transaction, wallet_group
from jolteon_checkpoint_localnet import Relay
from localnet import write_json
from native_bridge_localnet import wait_for


def scenario(args, origin, peer):
    fixture = Fixtures(args)
    genesis = origin.call("get_block_by_number", "0x0")
    assert genesis["header"]["hash"] == peer.call("get_block_hash", "0x0")
    initial, deps = genesis["transactions"][:2]
    index, funds = next((i, out) for i, out in enumerate(initial["outputs"]) if out["lock"]["args"] == FAUCET_ARG)
    secp = dependency(point(deps["hash"], 0), "dep_group")
    program = args.direct_checkpoint.read_bytes()
    occupied = (len(program) + 200) * CKB
    fee_funds = 8_000 * CKB
    remainder = int(funds["capacity"], 16) - occupied - fee_funds - FEE
    deployment = transaction([point(initial["hash"], index)],
        [output(capacity, fixture.standard) for capacity in (occupied, fee_funds, remainder)],
        [hex_bytes(program), "0x", "0x"], [secp])
    deployed = origin.send(fixture.sign("deploy-checkpoint", deployment, [wallet_group(0, [fixture.faucet])]))

    # Seed the second devnet past its old genesis through the normal block verifier.
    # The measured checkpoint transaction itself is sent only to the origin.
    tip_height = int(origin.call("get_tip_block_number"), 16)
    for height in range(1, tip_height + 1):
        block = origin.call("get_block_by_number", hex(height))
        block_hash = block["header"].pop("hash")
        for uncle in block["uncles"]:
            uncle["header"].pop("hash")
        for tx in block["transactions"]:
            tx.pop("hash")
        assert peer.call("submit_block", "0", block) == block_hash
    node = peer.call("local_node_info")
    origin.call("add_node", node["node_id"], f"/ip4/127.0.0.1/tcp/{args.relay_port + 1}")
    wait_for(lambda: origin.call("get_peers"), "CKB relay peer connection", seconds=20)
    tip = origin.call("get_tip_header")["hash"]
    wait_for(lambda: peer.call("get_tip_header")["hash"] == tip, "peer syncing checkpoint deployment", seconds=30)
    network_spec = args.stem_run / "genesis.json"
    genesis_spec = json.loads(network_spec.read_text())
    members = genesis_spec["development_validators"]
    stem_report = json.loads((args.stem_run / "jolteon-report.json").read_text())
    height = stem_report["proof_verification"]["height"]
    data = {"hash": hex_bytes(ckb_hash(program)), "id": ckb_hash(le(0, 8) + binary(deployed) + le(1, 4) + le(0, 8)),
            "code": dependency(point(deployed, 0)), "seed": point(deployed, 1),
            "genesis": genesis["header"]["hash"], "network": genesis_spec["network_id"]}
    policies = [tomllib.loads((node.home / "ckb.toml").read_text())["tx_pool"] for node in (origin, peer)]
    assert all(policy["max_tx_verify_cycles"] == 70_000_000 and policy["min_fee_rate"] == 1_000
               for policy in policies), policies
    observed = {"verified_setup_blocks": tip_height, "setup_tip": tip}

    def submit(signed):
        txid = signed["hash"]
        before = peer.call("get_transaction", txid)
        assert not before or before["tx_status"]["status"] == "unknown", before
        started = time.perf_counter()
        assert origin.call("send_transaction", signed["json"], "passthrough") == txid

        def seen_in_peer_pool():
            status = peer.call("get_transaction", txid)
            if not status:
                return None
            if status["tx_status"]["status"] == "rejected":
                raise AssertionError(status)
            return status if status["tx_status"]["status"] in ("pending", "proposed") else None

        observed["peer_pool"] = wait_for(seen_in_peer_pool, "native checkpoint arriving via CKB P2P", seconds=25)
        observed["relay_seconds"] = time.perf_counter() - started
        observed["peer_pool_detail"] = peer.call("get_pool_tx_detail_info", txid)
        for _ in range(12):
            peer.call("generate_block")
            status = peer.call("get_transaction", txid)["tx_status"]
            if status["status"] == "committed":
                break
            time.sleep(0.1)
        else:
            raise AssertionError(f"receiving CKB peer did not include {txid}")
        wait_for(lambda: origin.call("get_transaction", txid)["tx_status"]["status"] == "committed",
                 "origin seeing receiver's committed checkpoint", seconds=20)
        observed["peer_inclusion"] = peer.prove_committed(txid)
        observed["origin_inclusion"] = origin.prove_committed(txid)
        assert observed["origin_inclusion"] == observed["peer_inclusion"]
        return txid

    relay = Relay(args, origin, fixture, data, args.stem_run / f"node{len(members) - 1}" / "db", network_spec, members, None)
    relay.advance(height, secp, proof_file=args.stem_run / "two-chain.json", submit=submit)
    report = {"committee_size": len(members), "configuration": policies, "stem_header": stem_report["proof_verification"],
              "publication": relay.report(), "observations": observed}
    write_json(args.output / "ckb-relay-report.json", report)
    print(f"Relayed and confirmed {len(members)}-member checkpoint via CKB P2P: "
          f"{relay.publications[-1]['estimated_cycles']:,} cycles")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "direct-checkpoint", "stem-run", "output"):
        parser.add_argument("--" + name, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, default=22900)
    parser.add_argument("--relay-port", type=int, default=22910)
    args = parser.parse_args()
    args.output.mkdir(parents=True, mode=0o700)
    peer_output = args.output / "receiver"
    peer_output.mkdir(mode=0o700)
    origin = L1(args)
    try:
        peer = L1(SimpleNamespace(l1=args.l1, l1_port=args.relay_port, output=peer_output))
        try:
            scenario(args, origin, peer)
        finally:
            peer.stop()
    finally:
        origin.stop()


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Join a full replica, rotate weighted membership using a signed Cell, and continue committing."""

import argparse
import base64
import json
from pathlib import Path

from bridge_localnet import (
    CKB, SIGHASH_CODE, CkbRpc, dependency, output, point, reject_cli, script, sidechain_tx,
    transaction, wallet_group, write_key,
)
from localnet import (
    Rpc, await_genesis, config_value, execution_proof, export_execution_proof, init_network,
    run, start_gateway, stop_gateway, wait_height, write_json,
)


def sign(args, name, tx, key):
    raw, groups = args.output / f"{name}.json", args.output / f"{name}-sign.json"
    write_json(raw, tx)
    write_json(groups, [wallet_group(0, [key])])
    return json.loads(run(args.binary, "wallet", "sign", "--transaction", raw, "--groups", groups))


def verify_checkpoint(args, node, rpc, payment, handoffs):
    interval = args.checkpoint_interval
    height = (int(payment["height"]) // interval + 1) * interval
    wait_height(rpc, height + 1)
    tx_hash = base64.b64decode(payment["tx_result"]["data"]).hex()
    gateway = start_gateway(args, node)
    wallet = CkbRpc(args.gateway_port)
    try:
        proof = wallet.call("get_live_cell_proof", point("0x" + tx_hash, 0), hex(height))
    finally:
        wallet.close()
        stop_gateway(gateway)
    path = args.output / "live-cell-proof.json"
    write_json(path, proof)
    command = [args.proof_verifier, "-genesis", node.home / "config" / "genesis.json",
               "-cell-proof", path, "-height", str(height), "-out-point", tx_hash + "00000000"]
    rejected = reject_cli("header validator set differs", *command)
    handoff_file = args.output / "proof-handoffs.json"
    write_json(handoff_file, handoffs)
    verified = json.loads(run(*command, "-handoffs", handoff_file))
    proof["handoffs"] = handoffs
    write_json(path, proof)
    if json.loads(run(*command)) != verified:
        raise RuntimeError("Inline committee handoff produced a different Cell proof")
    return {"verified": verified, "missing_handoff_rejected": rejected}


def test(args):
    key = write_key(args.output, "governance", bytes([3] * 32))
    bad_key = write_key(args.output, "wrong-owner", bytes([4] * 32))
    pubkey = json.loads(run(args.binary, "wallet", "key", "--key-file", key))
    lock = script(SIGHASH_CODE, pubkey["blake160"])
    lock_path = args.output / "governance-lock.json"
    write_json(lock_path, lock)
    nodes, fixture = init_network(args, governance_lock=lock_path, observers=1)
    original_committee = nodes[:4]
    incoming = nodes[4]
    incoming.compact_transaction_history = args.checkpoint_compact_transactions
    if args.checkpoint_live_cell_replay:
        config_value(nodes[1].config, "indexer", '"null"')
    observer_rpc = Rpc(incoming.rpc)
    rpc = Rpc(nodes[0].rpc)
    try:
        for node in original_committee:
            node.start()
        await_genesis(original_committee)
        previous = sidechain_tx(nodes[0], fixture["transactions"][0])
        incoming.start()
        await_genesis([incoming])
        wait_height(observer_rpc, int(previous["height"]))
        caught_up = observer_rpc.call("tx", hash="0x" + previous["hash"])
        if caught_up["tx_result"]["data"] != previous["tx_result"]["data"]:
            raise RuntimeError("Joining full replica did not replay the committed payment")

        next_committee = nodes[:3] + [incoming]
        weights = [2, 2, 1, 3]
        members = [
            {"ed25519": "0x" + base64.b64decode(n.validator["pub_key"]["value"]).hex(), "power": weight}
            for n, weight in zip(next_committee, weights)
        ]
        path = args.output / "next-committee.json"
        write_json(path, members)
        data = json.loads(run(args.binary, "committee-data", "--members", path))
        governance_point = json.loads((args.output / "app-state.json").read_text())["governance"]
        tx = transaction([governance_point], [output(12000 * CKB, lock)], [data],
                         [dependency(point(fixture["deposit_dep_group"], 0), "dep_group")])
        rejected = sidechain_tx(nodes[0], sign(args, "unauthorized-change", tx, bad_key)["molecule"],
                                required=False)
        rotation = sidechain_tx(nodes[0], sign(args, "committee-change", tx, key)["molecule"])
        height = int(rotation["height"])
        wait_height(rpc, height + 2)
        wait_height(observer_rpc, height + 2)
        old = rpc.call("validators", height=height + 1)["validators"]
        new = rpc.call("validators", height=height + 2)["validators"]
        expected = {n.validator["address"]: weight for n, weight in zip(next_committee, weights)}
        if {v["address"]: int(v["voting_power"]) for v in new} != expected:
            raise RuntimeError("Consensus validator set differs from the authorized update")
        if nodes[3].validator["address"] not in {v["address"] for v in old}:
            raise RuntimeError("Consensus removed the signer before the activation height")
        handoff = rpc.call("commit", height=height + 1)
        write_json(args.output / "handoff-commit.json", handoff)
        write_json(args.output / "activated-validators.json", new)
        pending_upgrade = None
        if args.check_upgrade_quorum:
            directory = args.output / "pending-upgrade"
            directory.mkdir()
            execution_proof(nodes[0], {rotation["hash"].lower(): {"height": height}}, directory)
            pending_upgrade = reject_cli(
                "certificate announces a committee change", args.proof_verifier,
                "-genesis", nodes[0].home / "config" / "genesis.json",
                "-proof", directory / "execution-proof.json", "-height", str(height + 1),
                "-offline", nodes[2].validator["address"],
            )
        nodes[3].stop()
        next_tx = sidechain_tx(incoming, fixture["transactions"][1])
        wait_height(rpc, int(next_tx["height"]))
        execution_proof(nodes[0], {next_tx["hash"].lower(): {"height": int(next_tx["height"])}}, args.output)
        proof = args.output / "execution-proof.json"
        bundle = json.loads(proof.read_text())
        bundle["handoffs"] = [{"commit": handoff, "members": new}]
        write_json(proof, bundle)
        verified = run(args.proof_verifier, "-proof", proof, "-genesis",
                       nodes[0].home / "config" / "genesis.json")
        report = {"joining_replica": incoming.address, "replayed_height": int(previous["height"]),
                  "unauthorized_change_code": rejected["code"], "changed_at": height,
                  "effective_height": height + 2, "old_power": sum(int(v["voting_power"]) for v in old),
                  "new_power": sum(expected.values()), "post_rotation_payment_height": int(next_tx["height"]),
                  "incoming_signer": incoming.validator["address"], "execution_proof": verified}
        if pending_upgrade is not None:
            report["pending_upgrade_rejected"] = pending_upgrade
        if args.checkpoint_cell_proofs:
            report["live_cell_proof"] = verify_checkpoint(args, incoming, observer_rpc, next_tx, bundle["handoffs"])
        if args.checkpoint_compact_transactions:
            ckb_hash = "0x" + base64.b64decode(rotation["tx_result"]["data"])[:32].hex()
            stored = observer_rpc.call("abci_query", path='"/transaction"', data=ckb_hash)["response"]
            if stored["code"] != 0 or base64.b64decode(stored["value"])[32:] != base64.b64decode(rotation["tx"]):
                raise RuntimeError("Compact validator omitted the committee-change transaction")
            report["compact_governance_transaction_retained"] = ckb_hash
        if args.checkpoint_live_cell_replay:
            ckb_hash = base64.b64decode(next_tx["tx_result"]["data"])[:32].hex()
            path = args.output / "compact-execution-proof.json"
            request = export_execution_proof(nodes[1], int(next_tx["height"]), ckb_hash, path,
                                             args.output / "proof-handoffs.json")
            if run(args.proof_verifier, "-proof", path, "-genesis", nodes[0].home / "config" / "genesis.json") != verified:
                raise RuntimeError("Exported execution proof differs from the verified receipt")
            report["index_free_execution_proof"] = verified
            report["export_without_handoff_rejected"] = reject_cli("header validator set differs", *request[:-2])
        write_json(args.output / "governance.json", report)
        print(json.dumps(report, indent=2))
    finally:
        rpc.close()
        observer_rpc.close()
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "comet", "output", "proof-verifier"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--base-port", default=28600, type=int)
    parser.add_argument("--gateway-port", default=28700, type=int)
    parser.add_argument("--checkpoint-cell-proofs", action="store_true")
    parser.add_argument("--checkpoint-compact-transactions", action="store_true")
    parser.add_argument("--checkpoint-live-cell-replay", action="store_true")
    parser.add_argument("--check-upgrade-quorum", action="store_true")
    args = parser.parse_args()
    for name in ("binary", "comet", "output", "proof_verifier"):
        setattr(args, name, getattr(args, name).resolve())
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval = "2s"
    args.rate, args.seconds = 2, 1
    args.checkpoint_compact_transactions |= args.checkpoint_live_cell_replay
    args.checkpoint_cell_proofs |= args.checkpoint_compact_transactions
    if args.checkpoint_cell_proofs:
        args.checkpoint_interval = 4
    args.output.mkdir(mode=0o700)
    test(args)


if __name__ == "__main__":
    main()

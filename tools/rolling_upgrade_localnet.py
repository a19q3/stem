#!/usr/bin/env python3
"""Verify an application rolling upgrade and binary rollback on a weighted committee."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import time

from bridge_localnet import reject_cli, sidechain_tx
from localnet import Rpc, await_genesis, execution_proof, init_network, run, wait_height, write_json
from partition_localnet import receipts, wait_peer_sets


def signing_position(node):
    state = json.loads((node.home / "data" / "priv_validator_state.json").read_text())
    return tuple(int(state[field]) for field in ("height", "round", "step"))


def current_binary(node):
    app = node.processes[0]
    with Path(f"/proc/{app.pid}/exe").open("rb") as executable:
        return hashlib.file_digest(executable, "sha256").hexdigest()


def verified_payment(args, directory, node, peers, signed):
    directory.mkdir()
    payment = sidechain_tx(node, signed)
    receipt = payment["tx_result"]
    if int(receipt["gas_used"]) <= 0:
        raise RuntimeError("The fixture payment did not execute its CKB lock")
    height = receipts(peers, payment["hash"].lower())
    # Fetch the successor after its commit is recorded in the following block.
    rpc = Rpc(node.rpc)
    try:
        wait_height(rpc, height + 2)
    finally:
        rpc.close()
    execution_proof(node, {payment["hash"].lower(): {"height": height}}, directory)
    path = directory / "execution-proof.json"
    command = [args.proof_verifier, "-genesis", node.home / "config" / "genesis.json", "-proof", path]
    proof = json.loads(path.read_text())
    details = {"tx": payment["hash"], "height": height, "cycles": int(receipt["gas_used"]),
               "proof": str(path.relative_to(args.output)), "verified": run(*command)}
    return details, proof, command


def block_signers(result):
    return {signature["validator_address"] for signature in result["signed_header"]["commit"]["signatures"]
            if signature["block_id_flag"] == 2}


def common_block(rpcs, height):
    blocks = [rpc.call("block", height=height) for rpc in rpcs]
    observations = {(block["block_id"]["hash"], block["block"]["header"]["app_hash"]) for block in blocks}
    if len(observations) != 1:
        raise RuntimeError(f"Replicas disagree at height {height}")
    block_hash, app_hash = observations.pop()
    return {"height": height, "block_hash": block_hash, "app_hash": app_hash}


def scenario(args):
    nodes, fixture = init_network(args, voting_powers=args.voting_powers)
    rpcs = [Rpc(node.rpc) for node in nodes]
    ids = {node.address for node in nodes}
    full_mesh = [ids - {node.address} for node in nodes]
    txs = iter(fixture["transactions"])
    digests = {}
    for version, binary in (("before", args.binary), ("after", args.new_binary)):
        with binary.open("rb") as executable:
            digests[version] = hashlib.file_digest(executable, "sha256").hexdigest()
    if digests["before"] == digests["after"]:
        raise ValueError("Use two distinct application releases")

    try:
        for node in nodes:
            node.start()
        await_genesis(nodes)
        wait_peer_sets(rpcs, full_mesh)
        if any(current_binary(node) != digests["before"] for node in nodes):
            raise RuntimeError("The committee did not start on the old release")
        first, proof, command = verified_payment(args, args.output / "initial", nodes[0], rpcs, next(txs))
        first_height = first["height"] + 1
        first_address = nodes[0].validator["address"]
        wrong_height = reject_cli("expected successor height", *command,
                                  "-height", str(first_height + 1), "-offline", first_address)
        duplicate = reject_cli("appears twice", *command, "-height", str(first_height),
                               "-offline", f"{first_address},{first_address}")
        unknown = reject_cli("outside the certificate committee", *command,
                             "-height", str(first_height), "-offline", "00" * 20)
        signatures = proof["next_commit"]["signed_header"]["commit"]["signatures"]
        signature = next(sig for sig in signatures if sig["block_id_flag"] == 2)
        signature["signature"] = base64.b64encode(bytes(64)).decode()
        invalid = args.output / "tampered-proof.json"
        write_json(invalid, proof)
        tampered = reject_cli("verify commit at", *command[:-1], invalid,
                              "-height", str(first_height), "-offline", first_address)
        current = first
        total = sum(args.voting_powers)
        upgraded, skipped = [], []
        steps = []

        def change(node, version, binary):
            nonlocal current, command
            address = node.validator["address"]
            off = [*command, "-height", str(current["height"] + 1), "-offline", address]
            possible_power = total - int(node.validator["power"])
            if possible_power * 3 <= total * 2:
                skipped.append({"replica": node.number, "reason": reject_cli("certificate leaves voting power", *off)})
                return False
            preflight = json.loads(run(*off))
            node.stop()
            stopped_at = time.monotonic()
            cursor = signing_position(node)
            rpcs[node.number].close()
            online = [peer for peer in nodes if peer is not node]
            active = [rpcs[peer.number] for peer in online]
            stage = args.output / f"{len(steps)}-validator-{node.number}-{version}"
            stage.mkdir()
            offline, proof, _ = verified_payment(args, stage / "offline", online[0], active, next(txs))
            if address in block_signers(proof["commit"]) or address in block_signers(proof["next_commit"]):
                raise RuntimeError("The stopped validator signed a block containing a new payment or its successor")
            if signing_position(node) != cursor:
                raise RuntimeError("Signer state changed while both services were stopped")
            restart_at = time.monotonic()
            node.binary = binary
            node.start()
            await_genesis([node])
            wait_height(rpcs[node.number], offline["height"] + 1)
            receipts(rpcs, offline["tx"].lower())
            caught_up = time.monotonic()
            wait_peer_sets(rpcs, full_mesh)
            if current_binary(node) != digests[version]:
                raise RuntimeError("The restarted application is running the wrong release")
            current, proof, command = verified_payment(args, stage / "rejoined", node, rpcs, next(txs))
            signers = block_signers(proof["commit"]) | block_signers(proof["next_commit"])
            if address not in signers:
                raise RuntimeError("The restarted validator did not sign either certified block")
            resumed = signing_position(node)
            if resumed <= cursor:
                raise RuntimeError("The restarted validator did not advance its persisted signing position")
            steps.append({"replica": node.number, "address": address, "release": version,
                          "binary_sha256": digests[version], "preflight": preflight, "offline": offline,
                          "rejoined": current, "signer_stopped": cursor, "signer_resumed": resumed,
                          "offline_seconds": restart_at - stopped_at, "catch_up_seconds": caught_up - restart_at,
                          "common_block": common_block(rpcs, current["height"] + 1)})
            return True

        for node in nodes:
            if change(node, "after", args.new_binary):
                upgraded.append(node)
        if not upgraded:
            raise RuntimeError("The committee has no validator that can be rolled individually")
        rollback = upgraded[0]
        change(rollback, "before", args.binary)
        change(rollback, "after", args.new_binary)
        report = {"voting_powers": args.voting_powers, "releases": digests, "initial": first,
                  "wrong_height_rejected": wrong_height, "duplicate_rejected": duplicate,
                  "unknown_validator_rejected": unknown, "tampered_signature_rejected": tampered,
                  "skipped": skipped, "steps": steps,
                  "final_binary_sha256": [current_binary(node) for node in nodes]}
        write_json(args.output / "rolling-upgrade.json", report)
        print(json.dumps(report, indent=2))
    finally:
        for rpc in rpcs:
            rpc.close()
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "new-binary", "comet", "proof-verifier", "output"):
        parser.add_argument("--" + name, required=True, type=lambda path: Path(path).resolve())
    parser.add_argument("--base-port", default=29100, type=int)
    parser.add_argument("--voting-powers", nargs=4, default=[3, 3, 2, 2], type=int)
    args = parser.parse_args()
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "500ms", 13, 1
    args.checkpoint_interval = 8
    args.checkpoint_cell_proofs = args.checkpoint_compact_transactions = args.checkpoint_live_cell_replay = True
    args.compact_transaction_history = True
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

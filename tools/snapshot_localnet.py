#!/usr/bin/env python3
"""Catch up a fresh replica using authenticated application chunks and the P2P light client."""

import argparse
import base64
import json
from pathlib import Path
import re
import time

from bridge_localnet import CkbRpc, FEE, dependency, output, point, reject_cli, sidechain_tx, transaction, wallet_group, write_key
from governance_localnet import wait_height
from localnet import (
    Rpc, await_genesis, config_value, execution_proof, export_execution_proof, init_network,
    run, set_sync_config, start_gateway, stop_gateway, tip, write_json,
)


def query_tx(rpc, tx_hash):
    response = rpc.call("abci_query", path='"/transaction"', data="0x" + tx_hash)["response"]
    if response["code"] != 0 and response["log"] == "unknown transaction":
        return None
    if response["code"] != 0:
        raise RuntimeError(response)
    return base64.b64decode(response["value"])[32:]


def indexed_cell(rpc, tx_hash):
    point = "0x" + tx_hash + "00000000"
    live = rpc.call("abci_query", path='"/live_cell"', data=point)["response"]
    if live["code"] != 0:
        raise RuntimeError(live)
    lock = json.loads(base64.b64decode(live["value"]))["output"]["lock"]
    params = [{"script": lock, "script_type": "lock", "script_search_mode": "exact",
               "with_data": False}, "asc", "0x10", None]
    page = rpc.call("abci_query", path='"/rpc/cells"', data="0x" + json.dumps(params).encode().hex())["response"]
    if page["code"] != 0:
        raise RuntimeError(page)
    expected = {"tx_hash": "0x" + tx_hash, "index": "0x0"}
    if expected not in [cell["out_point"] for cell in json.loads(base64.b64decode(page["value"]))["objects"]]:
        raise RuntimeError(f"Wallet index lacks the live Cell {expected}")


def checkpoint_membership(rpc, tx_hash, height):
    key = height.to_bytes(8, "big").hex() + tx_hash + "00000000"
    response = rpc.call("abci_query", path='"/rpc/live_cell_proof"', data="0x" + key)["response"]
    if response["code"] != 0:
        raise RuntimeError(response)
    return json.loads(base64.b64decode(response["value"]))


def compact_status(args, node, tx_hash, height):
    gateway = start_gateway(args, node)
    rpc = CkbRpc(args.gateway_port)
    try:
        status = rpc.call("get_transaction", "0x" + tx_hash, "0x2")
        expected_status = "unknown" if height is None else "committed"
        expected_number = None if height is None else hex(height)
        if status["transaction"] is not None or status["tx_status"]["status"] != expected_status \
                or status["tx_status"]["block_number"] != expected_number:
            raise RuntimeError(f"Unexpected compact transaction response: {status}")
        return status["tx_status"]
    finally:
        rpc.close()
        stop_gateway(gateway)


def spend_restored_cell(args, source, origin, joining, fixture, tx_hash, header_hash):
    outpoint = "0x" + tx_hash + "00000000"
    live = joining.call("abci_query", path='"/live_cell"', data=outpoint)["response"]
    cell = json.loads(base64.b64decode(live["value"]))["output"]
    tx = transaction([point("0x" + tx_hash, 0)],
                     [output(int(cell["capacity"], 16) - FEE, cell["lock"])], ["0x"],
                     [dependency(point(fixture["deposit_dep_group"], 0), "dep_group")])
    tx["inputs"][0]["since"] = hex((1 << 63) | 2)
    tx["header_deps"] = [header_hash]
    directory = args.output / "restored-spend"
    directory.mkdir()
    private = write_key(directory, "dev-owner", bytes([2] * 32))
    transaction_file, groups_file = directory / "transaction.json", directory / "groups.json"
    write_json(transaction_file, tx)
    write_json(groups_file, [wallet_group(0, [private])])
    signed = json.loads(run(args.binary, "wallet", "sign", "--transaction", transaction_file, "--groups", groups_file))
    receipt = sidechain_tx(source, signed["molecule"])
    spent = joining.call("abci_query", path='"/live_cell"', data=outpoint)["response"]
    if spent["code"] == 0 or spent["log"] != "cell is not live":
        raise RuntimeError("A consumed Cell remains live after the restored transaction")
    execution_proof(origin, {receipt["hash"].lower(): {"height": int(receipt["height"])}}, directory)
    verified = run(args.proof_verifier, "-genesis", origin.home / "config" / "genesis.json",
                   "-proof", directory / "execution-proof.json")
    return {"hash": signed["hash"], "relative_since": tx["inputs"][0]["since"],
            "header_dependency": header_hash, "verified": verified}


def verify_membership(args, origin, fixture, tx_hash, height):
    gateway = start_gateway(args, origin)
    rpc = CkbRpc(args.gateway_port)
    proof_file = args.output / "live-cell-proof.json"
    try:
        proof = rpc.call("get_live_cell_proof", point("0x" + tx_hash, 0), hex(height))
        live = rpc.call("get_live_cell", point("0x" + tx_hash, 0), True)
        if rpc.call("get_live_cell_proof", point(fixture["funding_tx"], 0), hex(height)) is not None:
            raise RuntimeError("Spent genesis Cell appears in the checkpoint")
    finally:
        rpc.close()
        stop_gateway(gateway)
    write_json(proof_file, proof)
    outpoint = tx_hash + "00000000"
    command = [args.proof_verifier, "-genesis", origin.home / "config" / "genesis.json",
               "-cell-proof", proof_file, "-out-point", outpoint]
    verified = json.loads(run(*command, "-height", str(height)))
    packed = bytes.fromhex(verified["cell_output"][2:])
    capacity = int.from_bytes(packed[16:24], "little")
    if capacity != int(live["cell"]["output"]["capacity"], 16) or verified["data"] != live["cell"]["data"]["content"]:
        raise RuntimeError("Certified Cell differs from the wallet response")
    reject_cli("expected empty checkpoint", *command, "-height", str(height + 1))
    altered = json.loads(proof_file.read_text())
    membership = altered["checkpoint"]["membership"]
    record = bytearray(base64.b64decode(membership["record"]))
    record[-1] ^= 1
    membership["record"] = base64.b64encode(record).decode("ascii")
    bad_proof = args.output / "rejected-live-cell-proof.json"
    write_json(bad_proof, altered)
    command[command.index("-cell-proof") + 1] = bad_proof
    rejection = reject_cli("invalid Live Cell path", *command, "-height", str(height))
    report = {"verified": verified, "record_mutation_rejected": rejection,
              "wrong_height_rejected": True, "spent_cell_omitted": True}
    if proof["checkpoint"]["format"] >= 3:
        downgraded = json.loads(proof_file.read_text())
        downgraded["checkpoint"]["format"] -= 1
        downgrade_file = args.output / "rejected-format-downgrade.json"
        write_json(downgrade_file, downgraded)
        command[command.index("-cell-proof") + 1] = downgrade_file
        report["downgrade_rejected"] = reject_cli("checkpoint commitment differs", *command, "-height", str(height))
    return proof["checkpoint"], report


def scenario(args):
    nodes, fixture = init_network(args, observers=1)
    origin = nodes[0]
    joining = nodes[-1]
    joining.rpc_index = True
    compact = args.checkpoint_compact_transactions
    joining.compact_transaction_history = compact
    nodes[2].compact_transaction_history = compact
    pruning = nodes[1]
    pruning.retain_consensus_blocks = 8
    if args.checkpoint_live_cell_replay:
        config_value(pruning.config, "indexer", '"null"')
    rpc, sync_rpc = Rpc(origin.rpc), Rpc(joining.rpc)
    pruning_rpc = Rpc(pruning.rpc)
    try:
        for node in nodes[:-1]:
            node.start()
        await_genesis(nodes[:-1])
        tx = sidechain_tx(origin, fixture["transactions"][0])
        wait_height(rpc, 6)
        trust_height = 4
        trust_hash = rpc.call("block", height=trust_height)["block_id"]["hash"]
        set_sync_config(joining, nodes[:2], trust_height, trust_hash)
        started = time.monotonic()
        joining.start()
        await_genesis([joining])
        wait_height(sync_rpc, 6)
        installed = time.monotonic()
        logs = (joining.directory / "app.log").read_text()
        match = re.search(r"event=snapshot_installed height=(\d+)", logs)
        if not match:
            raise RuntimeError(f"Replica replayed from genesis. See {joining.directory}")
        checkpoint = int(match[1])
        ckb_tx_hash = base64.b64decode(tx["tx_result"]["data"]).hex()
        expected = base64.b64decode(tx["tx"])
        expected_history = None if compact else expected
        if query_tx(sync_rpc, ckb_tx_hash) != expected_history:
            raise RuntimeError("Restored transaction history differs from the checkpoint profile")
        compact_report = {}
        if compact:
            initial_height = int(tx["height"])
            status = compact_status(args, joining, ckb_tx_hash, initial_height)
            duplicate = sync_rpc.broadcast_packed(expected)
            rejection = "Unknown(" if args.checkpoint_live_cell_replay else "transaction already exists"
            if duplicate["code"] == 0 or rejection not in duplicate["log"]:
                raise RuntimeError(f"Compact checkpoint failed the transaction replay check: {duplicate}")
            compact_report = {"committed_status_after_sync": status, "duplicate_rejection": duplicate["log"]}
        indexed_cell(sync_rpc, ckb_tx_hash)
        proof_report = {}
        if args.checkpoint_cell_proofs:
            proof, proof_report = verify_membership(args, origin, fixture, ckb_tx_hash, checkpoint)
            if checkpoint_membership(sync_rpc, ckb_tx_hash, checkpoint) != proof:
                raise RuntimeError("Snapshot restore rebuilt a different Cell proof")
            if compact:
                replica = Rpc(nodes[2].rpc)
                try:
                    if query_tx(replica, ckb_tx_hash) is not None or checkpoint_membership(replica, ckb_tx_hash, checkpoint) != proof:
                        raise RuntimeError("Archive and compact validators produced different canonical checkpoints")
                finally:
                    replica.close()
                compact_report["archive_and_compact_checkpoints_agree"] = True
        second = sidechain_tx(origin, fixture["transactions"][1])
        wait_height(sync_rpc, int(second["height"]))
        joining.stop(crash=True)
        sync_rpc.close()
        joining.start()
        await_genesis([joining])
        wait_height(sync_rpc, int(second["height"]))
        if query_tx(sync_rpc, ckb_tx_hash) != expected_history:
            raise RuntimeError("Restored archive did not survive restart")
        indexed_cell(sync_rpc, ckb_tx_hash)
        if args.checkpoint_cell_proofs:
            if checkpoint_membership(sync_rpc, ckb_tx_hash, checkpoint) != proof:
                raise RuntimeError("Cell proof became unavailable after restart")
            proof_report["verified_after_restore_and_restart"] = True
        if compact:
            second_hash = base64.b64decode(second["tx_result"]["data"]).hex()
            compact_report["committed_status_after_restart"] = compact_status(
                args, joining, second_hash, int(second["height"]))
        wait_height(pruning_rpc, 13)
        earliest = int(pruning_rpc.call("status")["sync_info"]["earliest_block_height"])
        if earliest <= 1 or query_tx(pruning_rpc, ckb_tx_hash) != expected:
            raise RuntimeError("Consensus pruning did not preserve the CKB archive")
        archive_base = int(rpc.call("status")["sync_info"]["earliest_block_height"])
        if archive_base != 1:
            raise RuntimeError("Archive replica pruned consensus history")
        if compact:
            pruning.stop()
            pruning_rpc.close()
            pruning.compact_transaction_history = True
            pruning.start()
            await_genesis([pruning])
            if query_tx(pruning_rpc, ckb_tx_hash) is not None:
                raise RuntimeError("Compacting existing history left an old transaction body queryable")
            compact_report["committed_status_after_local_prune"] = compact_status(args, pruning, ckb_tx_hash, initial_height)
            if query_tx(rpc, ckb_tx_hash) != expected:
                raise RuntimeError("Archive replica lost the original witnessed transaction")
            compact_report["spend_from_restored_checkpoint"] = spend_restored_cell(
                args, joining, origin, sync_rpc, fixture, ckb_tx_hash, status["block_hash"])
            if args.checkpoint_live_cell_replay:
                compact_report["spent_status_after_snapshot"] = compact_status(args, joining, ckb_tx_hash, None)
                if query_tx(rpc, ckb_tx_hash) != expected:
                    raise RuntimeError("Archive replica lost the original spent transaction")
                spend = compact_report["spend_from_restored_checkpoint"]
                archive_receipt = json.loads((args.output / "restored-spend/execution-proof.json").read_text())["tx"]
                try:
                    pruning_rpc.call("tx", hash="0x" + archive_receipt["hash"])
                except RuntimeError as error:
                    if "indexing is disabled" not in str(error):
                        raise
                else:
                    raise RuntimeError("The compact validator still has a CometBFT transaction index")
                path = args.output / "compact-execution-proof.json"
                request = export_execution_proof(pruning, int(archive_receipt["height"]), spend["hash"], path)
                verified = run(args.proof_verifier, "-genesis", origin.home / "config" / "genesis.json", "-proof", path)
                if verified != spend["verified"] or "tx_index.db" in pruning.storage():
                    raise RuntimeError("Execution proof or index-free storage differs from the expected profile")
                request[request.index("-ckb-tx") + 1] = "00" * 32
                wrong_hash = reject_cli("no successful receipt", *request)
                request[request.index("-height") + 1] = str(initial_height)
                missing_block = reject_cli(f"block at height {initial_height}:", *request)
                compact_report["no_comet_tx_index"] = {"verified": verified, "unknown_hash": wrong_hash,
                                                        "pruned_block": missing_block}
        state = tip(joining)
        report = {"snapshot_height": checkpoint, "trusted_height": trust_height,
                  "sync_seconds": installed - started, "archived_transaction_height": int(tx["height"]),
                  "pruned_replica_base": earliest, "archive_replica_base": archive_base,
                  "post_sync_transaction_height": int(second["height"]), "tip_after_restart": state,
                  "wallet_index_verified_after_snapshot_and_restart": True}
        if proof_report:
            report["live_cell_proof"] = proof_report
        if compact_report:
            report["compact_transaction_history"] = compact_report
        write_json(args.output / "snapshot-sync.json", report)
        print(json.dumps(report, indent=2))
    finally:
        rpc.close()
        sync_rpc.close()
        pruning_rpc.close()
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "comet", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--base-port", default=29500, type=int)
    parser.add_argument("--gateway-port", default=29600, type=int)
    parser.add_argument("--proof-verifier", type=lambda path: Path(path).resolve())
    parser.add_argument("--checkpoint-compact-transactions", action="store_true")
    parser.add_argument("--checkpoint-live-cell-replay", action="store_true")
    args = parser.parse_args()
    for name in ("binary", "comet", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval = "2s"
    args.rate, args.seconds = 2, 1
    args.checkpoint_cell_proofs = args.proof_verifier is not None
    args.checkpoint_compact_transactions |= args.checkpoint_live_cell_replay
    if args.checkpoint_compact_transactions and not args.checkpoint_cell_proofs:
        parser.error("Compact checkpoints require --proof-verifier")
    if args.checkpoint_cell_proofs:
        args.rate = 258
    args.checkpoint_interval = 4
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

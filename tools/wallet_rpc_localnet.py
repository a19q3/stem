#!/usr/bin/env python3
"""Test the stock CCC client and the CKB RPC index against four BFT validators."""

import argparse
import hashlib
import http.client
import json
from pathlib import Path

from bridge_localnet import SIGHASH_CODE, binary, sidechain_tx, wait_sidechain_tx
from localnet import Rpc, await_genesis, execution_proof, init_network, run, start_gateway, stop_gateway, write_json


def scenario(args):
    nodes, fixture = init_network(args)
    gateway = None
    try:
        for node in nodes:
            node.start()
        await_genesis(nodes)
        initial = sidechain_tx(nodes[0], fixture["transactions"][0])
        # Rebuild at the tip of an existing DB with one of the four funding Cells spent.
        nodes[0].stop()
        nodes[0].rpc_index = True
        nodes[0].start()
        await_genesis(nodes)
        gateway = start_gateway(args, nodes[0])
        url = f"http://127.0.0.1:{args.gateway_port}/"
        config = {
            "rpc": url, "sighashCode": SIGHASH_CODE,
            "sighashDep": {"txHash": fixture["deposit_dep_group"], "index": 0},
        }
        write_json(args.output / "sdk-config.json", config)
        result = json.loads(run("node", Path(__file__).with_name("wallet_rpc_ccc.cjs"),
                                args.sdk_prefix, args.output / "sdk-config.json"))
        comet_id = hashlib.sha256(binary(result["packed"])).hexdigest()
        execution_proof(nodes[0], {comet_id: {"height": int(result["blockNumber"])}}, args.output)
        trusted = nodes[0].home / "config" / "genesis.json"
        result["proof_verification"] = run(args.proof_verifier, "-genesis", trusted,
                                           "-proof", args.output / "execution-proof.json")
        result["index_rebuilt_after_height"] = int(initial["height"])
        for node in nodes:
            rpc = Rpc(node.rpc)
            try:
                wait_sidechain_tx(rpc, comet_id)
            finally:
                rpc.close()
        result["replicas"] = len(nodes)

        connection = http.client.HTTPConnection("127.0.0.1", args.gateway_port, timeout=5)
        try:
            calls = [
                {"jsonrpc": "2.0", "method": "get_block_hash", "params": [hex(int(result["blockNumber"]))], "id": 1},
                {"jsonrpc": "2.0", "method": "get_transaction", "params": [result["hash"], "0x0", True], "id": 2},
                {"jsonrpc": "2.0", "method": "get_tip_block_number", "params": [], "id": 3},
                {"jsonrpc": "2.0", "method": "get_block", "params": [result["blockHash"], "0x2", True], "id": 4},
                {"jsonrpc": "2.0", "method": "get_block_by_number", "params": ["0x0"], "id": 5},
            ]
            connection.request("POST", "/", json.dumps(calls), {"Content-Type": "application/json"})
            reply = {item["id"]: item["result"] for item in json.loads(connection.getresponse().read())}
            if reply[1] != result["blockHash"] or reply[2]["transaction"] != result["packed"]:
                raise RuntimeError("CKB header or packed transaction lookup differs from the executed payment")
            block = reply[4]["block"]
            index = next(index for index, tx in enumerate(block["transactions"]) if tx["hash"] == result["hash"])
            if int(reply[4]["cycles"][index], 16) <= 1_000_000 or reply[5]["header"]["number"] != "0x0":
                raise RuntimeError("CKB block or cycle data differs from the valid wallet payment")
            result["gateway_cycles"] = int(reply[4]["cycles"][index], 16)
        finally:
            connection.close()
        write_json(args.output / "wallet-rpc.json", result)
        print(json.dumps({key: value for key, value in result.items() if key != "packed"}, indent=2))
    finally:
        if gateway:
            stop_gateway(gateway)
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "proof-verifier", "sdk-prefix", "output"):
        parser.add_argument("--" + flag, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--base-port", type=int, default=32000)
    parser.add_argument("--gateway-port", type=int, default=32100)
    args = parser.parse_args()
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "2s", 4, 1
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

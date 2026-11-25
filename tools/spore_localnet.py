#!/usr/bin/env python3
"""Run the published Spore V2 ELF with CCC against a local four-validator network."""

import argparse
import hashlib
import json
from pathlib import Path
import time

from bridge_localnet import (
    CKB, FEE, SIGHASH_CODE, ckb_hash, dependency, hex_bytes, output, point, script,
    sidechain_tx, transaction, write_key,
)
from localnet import Rpc, await_genesis, execution_proof, init_network, run, start_gateway, stop_gateway, write_json


SPORE_CODE = "0x4a4dce1df3dffff7f8b2cd7dff7303df3b6150c9788cb75dcf6747247132b9f5"
SPORE_DEPLOYMENT = "0x96b198fb5ddbd1eed57ed667068f1f1e55d07907b4c0dbd38675a69ea1b69824"


def deploy(args, node, fixture, elf):
    key = write_key(args.output, "public-dev-key", bytes([1]) * 32)
    public = json.loads(run(args.binary, "wallet", "key", "--key-file", key))
    owner = script(SIGHASH_CODE, public["blake160"])
    dep = dependency(point(fixture["deposit_dep_group"], 0), "dep_group")
    code_capacity = (len(elf) + 100) * CKB
    change = args.rate * 100 * CKB - FEE - code_capacity
    if change < 200 * CKB:
        raise RuntimeError("Fixture capacity cannot fund code and the Spore lifecycle")
    deploy_tx = transaction(
        [point(fixture["funding_tx"], index) for index in range(args.rate)],
        [output(code_capacity, owner), output(change, owner)],
        [hex_bytes(elf), "0x"], [dep],
    )
    write_json(args.output / "deploy.json", deploy_tx)
    write_json(args.output / "deploy-groups.json",
               [{"inputs": list(range(args.rate)), "keys": [str(key)]}])
    signed = json.loads(run(args.binary, "wallet", "sign", "--transaction", args.output / "deploy.json",
                            "--groups", args.output / "deploy-groups.json"))
    receipt = sidechain_tx(node, signed["molecule"])
    return signed["hash"], receipt


def wait_replica(node, tx):
    rpc = Rpc(node.rpc)
    try:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                receipt = rpc.call("tx", hash="0x" + tx["id"])
            except RuntimeError as error:
                if "not found" not in str(error):
                    raise
                time.sleep(0.1)
                continue
            if receipt["tx_result"]["code"] != 0 or int(receipt["height"]) != tx["height"]:
                raise RuntimeError(f"Unexpected replica {node.number} receipt: {receipt}")
            return receipt["tx_result"]
        raise TimeoutError(f"Replica {node.number} did not finalize {tx['hash']}")
    finally:
        rpc.close()


def scenario(args):
    elf = args.spore.read_bytes()
    if hex_bytes(ckb_hash(elf)) != SPORE_CODE:
        raise RuntimeError("Spore ELF does not match the published V2 mainnet deployment")
    nodes, fixture = init_network(args)
    gateway = None
    try:
        nodes[0].rpc_index = True
        for node in nodes:
            node.start()
        await_genesis(nodes)
        deployed, receipt = deploy(args, nodes[0], fixture, elf)
        gateway = start_gateway(args, nodes[0])
        config = {
            "rpc": f"http://127.0.0.1:{args.gateway_port}/",
            "sighashCode": SIGHASH_CODE, "sighashDep": {"txHash": fixture["deposit_dep_group"], "index": 0},
            "sporeCode": SPORE_CODE, "sporeDep": {"txHash": deployed, "index": 0},
            "funding": {"txHash": deployed, "index": 1},
        }
        write_json(args.output / "ccc-config.json", config)
        flow = json.loads(run("node", Path(__file__).with_name("spore_ccc.cjs"),
                              args.sdk_prefix, args.output / "ccc-config.json"))
        proofs = {}
        for action in ("minted", "transferred", "melted"):
            tx = flow[action]
            results = [wait_replica(node, tx) for node in nodes]
            if any(result != results[0] for result in results):
                raise RuntimeError(f"Replica execution results differ for {action}")
            tx["cycles"] = int(results[0]["gas_used"])
            directory = args.output / action
            directory.mkdir()
            execution_proof(nodes[0], {tx["id"]: tx}, directory)
            proofs[action] = run(args.proof_verifier, "-proof", directory / "execution-proof.json",
                                 "-genesis", nodes[0].home / "config" / "genesis.json")
        report = {
            **flow,
            "spore_version": "0.2.2-beta.1", "code_hash": SPORE_CODE,
            "mainnet_code_out_point": point(SPORE_DEPLOYMENT, 0),
            "elf_sha256": hashlib.sha256(elf).hexdigest(),
            "sidechain_code_out_point": point(deployed, 0), "deploy_height": int(receipt["height"]),
            "replicas": args.replicas, "proof_verification": proofs,
        }
        write_json(args.output / "spore-lifecycle.json", report)
        print(json.dumps(proofs, indent=2))
    finally:
        if gateway:
            stop_gateway(gateway)
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "proof-verifier", "spore", "sdk-prefix", "output"):
        parser.add_argument("--" + flag, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--base-port", type=int, default=31300)
    parser.add_argument("--gateway-port", type=int, default=31400)
    args = parser.parse_args()
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "2s", 800, 1
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

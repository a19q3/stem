#!/usr/bin/env python3
"""Execute the published CellScript 2027 equality Lock on four sidechain validators."""

import argparse
import hashlib
import http.client
import json
from pathlib import Path
import shutil
import subprocess

from bridge_localnet import (
    CKB, FEE, SIGHASH_CODE, binary, ckb_hash, dependency, hex_bytes, le, output,
    point, script, sidechain_tx, transaction, wait_sidechain_tx, write_key,
)
from localnet import Rpc, await_genesis, execution_proof, init_network, run, start_gateway, stop_gateway, write_json


def signed(args, name, tx, groups):
    source = args.output / f"{name}.json"
    signers = args.output / f"{name}-groups.json"
    write_json(source, tx)
    write_json(signers, groups)
    return json.loads(run(args.binary, "wallet", "sign", "--transaction", source, "--groups", signers))


def witness(args, package, claimed, name):
    path = args.output / f"{name}.bin"
    abi = json.loads(run(args.cellc, "entry-witness", package, "--lock", "unlock",
                         "--target", "riscv64-elf", "--target-profile", "ckb",
                         "--arg", hex_bytes(claimed), "--output", path, "--json"))
    if abi["witness_args_field"] != "input_type":
        raise RuntimeError("Unexpected CellScript witness placement")
    payload = path.read_bytes()
    size = 20 + len(payload)
    # Standard Molecule WitnessArgs, with only the input_type BytesOpt present.
    return hex_bytes(le(size, 4) + le(16, 4) + le(16, 4) + le(size, 4) + le(len(payload), 4) + payload)


def ckb_rpc(connection, method, params):
    body = {"jsonrpc": "2.0", "method": method, "params": params, "id": 1}
    connection.request("POST", "/", json.dumps(body), {"Content-Type": "application/json"})
    reply = connection.getresponse()
    message = json.loads(reply.read())
    if reply.status != 200:
        raise RuntimeError(f"CKB RPC returned HTTP {reply.status}: {message}")
    return message


def build(args):
    package = args.output / "cellscript-package"
    shutil.copytree(args.package, package, ignore=shutil.ignore_patterns("build"))
    subprocess.run([args.cellc, "build", "--locked", "--offline", "--production",
                    "--target", "riscv64-elf", "--target-profile", "ckb", "--entry-lock", "unlock"],
                   cwd=package, check=True, timeout=40, stdout=subprocess.DEVNULL)
    artifact = package / "build" / "main.elf"
    verification = json.loads(run(args.cellc, "verify-artifact", artifact, "--verify-sources", "--json"))
    if verification["binding_verification"] != "verified":
        raise RuntimeError("CellScript checker did not verify the source binding")
    return artifact.read_bytes(), verification, package


def scenario(args):
    elf, verification, package = build(args)
    code_hash = hex_bytes(ckb_hash(elf))
    owner = bytes([42]) * 32
    protected = script(code_hash, hex_bytes(owner), "data2")
    nodes, fixture = init_network(args)
    gateway = None
    connection = None
    try:
        nodes[0].rpc_index = True
        for node in nodes:
            node.start()
        await_genesis(nodes)
        gateway = start_gateway(args, nodes[0])
        connection = http.client.HTTPConnection("127.0.0.1", args.gateway_port, timeout=15)

        key = write_key(args.output, "public-dev-key", bytes([1]) * 32)
        public = json.loads(run(args.binary, "wallet", "key", "--key-file", key))
        normal = script(SIGHASH_CODE, public["blake160"])
        deps = [dependency(point(fixture["deposit_dep_group"], 0), "dep_group")]
        program = (len(elf) + 150) * CKB
        locked = 500 * CKB
        change = args.rate * 100 * CKB - program - locked - FEE
        if change < 61 * CKB:
            raise RuntimeError("CellScript artifact is too large for this fixture")
        deploy = transaction(
            [point(fixture["funding_tx"], i) for i in range(args.rate)],
            [output(program, normal), output(locked, protected), output(change, normal)],
            [hex_bytes(elf), hex_bytes(owner), "0x"], deps,
        )
        code = signed(args, "deploy", deploy, [{"inputs": list(range(args.rate)), "keys": [str(key)]}])
        deployed = sidechain_tx(nodes[0], code["molecule"])

        locked_point = point(code["hash"], 1)
        spend = transaction([locked_point], [output(locked - FEE, normal)],
                            ["0x"], [dependency(point(code["hash"], 0))])
        spend["witnesses"] = [witness(args, package, bytes([43]) * 32, "wrong-witness")]
        bad = signed(args, "wrong-witness", spend, [])
        error = ckb_rpc(connection, "send_transaction", [bad["json"], "passthrough"]).get("error", {})
        reason = error.get("data", "")
        if error.get("code") != -302 or "ValidationFailure" not in reason or f"{code_hash[2:]}.html#5" not in reason:
            raise RuntimeError(f"Expected CellScript assertion failure, got {error}")
        spend["witnesses"] = [witness(args, package, owner, "valid-witness")]
        good = signed(args, "valid-spend", spend, [])
        if good["hash"] != bad["hash"] or good["molecule"] == bad["molecule"]:
            raise RuntimeError("Two witnesses must share the same raw CKB transaction")
        reply = ckb_rpc(connection, "send_transaction", [good["json"], "passthrough"])
        if reply.get("result") != good["hash"]:
            raise RuntimeError(f"CellScript spend was rejected: {reply}")
        comet_id = hashlib.sha256(binary(good["molecule"])).hexdigest()
        receipts = []
        for node in nodes:
            rpc = Rpc(node.rpc)
            try:
                receipts.append(wait_sidechain_tx(rpc, comet_id))
            finally:
                rpc.close()
        height = int(receipts[0]["height"])
        if any(result["tx_result"] != receipts[0]["tx_result"] or int(result["height"]) != height
               for result in receipts):
            raise RuntimeError("CellScript receipts differ across validators")
        if ckb_rpc(connection, "get_live_cell", [locked_point, False])["result"]["status"] != "unknown":
            raise RuntimeError("Consumed Cell still appears live")
        execution_proof(nodes[0], {comet_id: {"height": height}}, args.output)
        proof = run(args.proof_verifier, "-genesis", nodes[0].home / "config" / "genesis.json",
                    "-proof", args.output / "execution-proof.json")
        report = {
            "compiler": run(args.cellc, "--version"),
            "compiler_sha256": hashlib.sha256(args.cellc.read_bytes()).hexdigest(),
            "source_content_hash": verification["source_content_hash"],
            "elf_sha256": hashlib.sha256(elf).hexdigest(),
            "artifact_hash": verification["artifact_hash"],
            "code_hash": code_hash, "hash_type": "data2",
            "deployed": code["hash"], "deploy_height": int(deployed["height"]),
            "spend": good["hash"], "height": height, "replicas": len(nodes),
            "cycles": int(receipts[0]["tx_result"]["gas_used"]),
            "rejection": error, "proof_verification": proof,
        }
        write_json(args.output / "cellscript-lock.json", report)
        print(json.dumps(report, indent=2))
    finally:
        if connection:
            connection.close()
        if gateway:
            stop_gateway(gateway)
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "proof-verifier", "cellc", "package", "output"):
        parser.add_argument("--" + flag, type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--base-port", type=int, default=32200)
    parser.add_argument("--gateway-port", type=int, default=32300)
    args = parser.parse_args()
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "2s", 40, 1
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

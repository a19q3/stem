"""Reopen each node of the four-validator mixed-version fixture with the other binary."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[2] / "tools"))
from bridge_localnet import CKB, hex_bytes, le, point
from jolteon_bridge_localnet import Client
from localnet import run, write_json
from native_bridge_localnet import cell, start, stem_id, wait_for
from wasm_sdk_fixture import authenticate, observe, packed


def read(path):
    return json.loads(path.read_bytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("output", "reference", "candidate", "archive"):
        parser.add_argument("--" + flag, required=True, type=lambda s: Path(s).resolve())
    args = parser.parse_args()
    root = args.output
    report = read(root / "jolteon-report.json")
    genesis, peers_file = root / "genesis.json", root / "replicas.json"
    peers = read(peers_file)["peers"]
    old = list(map(Path, report["replica_binaries"]))
    swapped = [args.reference if binary == args.candidate else args.candidate for binary in old]
    assert len(old) == 4 and set(old) == {args.reference, args.candidate}
    args.binary = args.reference
    network = read(genesis)["network_id"]
    clients = [Client(args, int(peer["address"].rsplit(":", 1)[1]), network) for peer in peers]
    keys = {Ed25519PrivateKey.from_private_bytes(p.read_bytes()).public_key().public_bytes_raw().hex(): p
            for p in root.glob("validator-*.key")}
    payments = read(root / "signed-workload.json")
    source = point(stem_id(args, network, payments[0]), 0)
    template = read(root / "payments/0-request.json")
    code = read(root / "payments/0-context.json")["cells"][1]
    request = {**template, "inputs": [{"out_point": source}], "outputs": [cell(98 * CKB, read(root / "payment-lock.json"))]}
    write_json(root / "resume-request.json", request)
    write_json(root / "resume-context.json", {"cells": [{"out_point": source, "cell": template["outputs"][0]}, code]})
    write_json(root / "resume-draft.json", json.loads(run(args.binary, "wallet", "build", "--request", root / "resume-request.json")))
    signed = json.loads(run(args.binary, "wallet", "sign", "--genesis", genesis, "--transaction", root / "resume-draft.json",
                            "--context", root / "resume-context.json", "--group-index", "0", "--lock", root / "payment-lock.json",
                            "--key-file", root / "payment-key.key"))
    tx_id = stem_id(args, network, signed)
    archive = clients[-1]

    def witness(out_point):
        height, record = observe(archive, "/stem/cell", data=packed(out_point))
        return {**authenticate(archive, height), **record, "point": packed(out_point)}

    children = []
    try:
        for i, (binary, peer) in enumerate(zip(swapped, peers, strict=True)):
            cmd = [binary, "run", "--home", root / f"node{i}", "--configuration", peers_file,
                   "--genesis", genesis, "--key-file", keys[peer["key"]]]
            children.append(start(args, cmd + (["--archive"] if i == 3 else []), f"node{i}-swapped.log"))
        wait_for(lambda: all(c.status()["committed_transactions"] == len(payments) for c in clients), "all reopened databases")
        previous = report["agreement"]
        assert all(c.header(previous["height"]) == previous for c in clients)
        before = witness(source)
        assert before["cell"]
        result = archive.call("broadcast_tx_commit", tx=signed)
        assert result["hash"] == tx_id.removeprefix("0x")
        height = result["height"]
        wait_for(lambda: all(c.status()["height"] > height for c in clients), "common height after new spend")
        assert all(c.header(height) == archive.header(height) for c in clients)
        index = archive.locations[result["hash"]]["index"]
        _, execution = observe(archive, "/stem/execution", height=height, data=hex_bytes(le(index, 4)))
        after, successor = witness(source), witness(point(tx_id, 0))
        assert after["cell"] is None and successor["cell"]
        ev = {"genesis": read(genesis), "trust": json.loads(run(args.binary, "jolteon-trust", "--genesis", genesis)),
              "workload": payments, "report": report, "previous": before, "spent": after, "successor": successor,
              "execution": {**authenticate(archive, height), **execution, "id": tx_id, "signed": signed},
              "reopened_binaries": [hashlib.sha256(b.read_bytes()).hexdigest() for b in swapped]}
        with lzma.open(args.archive, "xt") as dest:
            json.dump(ev, dest, separators=(",", ":"))
        print("Swapped all four databases and certified a new spend at", height)
    finally:
        for child in children:
            child.terminate()
        for child in children:
            child.wait()


if __name__ == "__main__":
    main()

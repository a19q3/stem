"""Collect witnessed public member, agent, proxy and collection histories at the member mint."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "beta2"))
from cluster_index import batch
from replay import binary, ckb_hash, hex_bytes, script_hash, verify

AGENT = "0x923e997654b2697ee3f77052cb884e98f28799a4270fd412c3edb8f3987ca622"
PROXY = "0x4349889bda064adab8f49f7dd8810d217917f7df28e9b2a1df0b74442399670a"
CLUSTER = "0x0bbe768b519d8ea7b96d58f1182eb7e6ef96c541fbd9526975077ee09f049058"
SPORE = "0x685a60219309029d01310311dba953d67029170ca4848a4ff638e57002130a0d"
MINTS = (
    "0x383ae0aa442440b3a53a0e220f9f0dbccdc15ce4bf58996f1f77d19eef90e42f",
    "0x243ca8230b0b1a928e33936d5bd4ac95e10fd9d9ec28188a7741f9ec81d7ba44",
    "0x90623aff43799446b98c6a7ea110d4e41462df449a256147f852c337b0d6edba",
    "0x1f8b47ad0ee32b3c7287317e08303387e99e9bab40ac52cc0d43adece60c90a1",
)


class Chain:
    def __init__(self, rpc):
        self.rpc = rpc
        self.transactions = {}

    def fetch(self, hashes):
        needed = list(dict.fromkeys(h for h in hashes if h not in self.transactions))
        if needed:
            records = batch(self.rpc, [("get_transaction", [h]) for h in needed])
            for h, record in zip(needed, records, strict=True):
                assert record["tx_status"]["status"] == "committed" and record["transaction"]["hash"] == h
                self.transactions[h] = record

    def cells(self, points):
        self.fetch(p["tx_hash"] for p in points)
        cells = []
        for point in points:
            tx = self.transactions[point["tx_hash"]]["transaction"]
            i = int(point["index"], 16)
            cells.append({"point": point, "output": tx["outputs"][i], "data": tx["outputs_data"][i]})
        return cells

    def inputs(self, tx):
        return self.cells([i["previous_output"] for i in tx["inputs"]])

    def deps(self, tx):
        return self.cells([d["out_point"] for d in tx["cell_deps"] if d["dep_type"] == "code"])

    def birth(self, cell):
        identity = cell["output"]["type"]
        while True:
            tx = self.transactions[cell["point"]["tx_hash"]]["transaction"]
            previous = [c for c in self.inputs(tx) if c["output"]["type"] == identity]
            if not previous:
                return cell
            cell, = previous


def deployed(cell, code):
    ty = cell["output"]["type"]
    return ty is not None and ty["hash_type"] == "data1" and ty["code_hash"] == code


def claim(tx_hash, cell):
    output = cell["output"]
    return {"transaction": tx_hash, "out_point": cell["point"], "data_hash": hex_bytes(ckb_hash(binary(cell["data"]))),
            "lock_hash": hex_bytes(script_hash(output["lock"])), "type_hash": hex_bytes(script_hash(output["type"]))}


def collect(chain, stem, work, tx_hash):
    chain.fetch([tx_hash])
    rec = chain.transactions[tx_hash]
    tx = rec["transaction"]
    header, = batch(chain.rpc, [("get_header_by_number", [rec["tx_status"]["block_number"]])])
    assert header["hash"] == rec["tx_status"]["block_hash"]
    outputs = chain.cells([{"tx_hash": tx_hash, "index": hex(i)} for i in range(len(tx["outputs"]))])
    member, = [c for c in outputs if deployed(c, SPORE)]
    following, = [c for c in outputs if deployed(c, AGENT)]
    consumed, = [c for c in chain.inputs(tx) if deployed(c, AGENT)]
    member_deps = chain.deps(tx)
    cluster, = [c for c in member_deps if deployed(c, CLUSTER)]
    assert consumed in member_deps and following["output"]["type"] == consumed["output"]["type"]
    agent_birth = chain.birth(following)
    issued = chain.transactions[agent_birth["point"]["tx_hash"]]["transaction"]
    proxy, = [c for c in chain.deps(issued) if deployed(c, PROXY)
              and hex_bytes(script_hash(c["output"]["type"])) == agent_birth["data"]]
    proxy_birth = chain.birth(proxy)
    proxy_tx = chain.transactions[proxy_birth["point"]["tx_hash"]]["transaction"]
    collection_refs = [c for c in chain.deps(proxy_tx) if c["output"]["type"] == cluster["output"]["type"]]
    collection_inputs = [c for c in chain.inputs(proxy_tx) if c["output"]["type"] == cluster["output"]["type"]]
    assert collection_refs or collection_inputs
    locations = {"member": member, "agent": following, "proxy": proxy, "collection": cluster,
                 "spent_agent": consumed, "proxy_birth_collection_deps": collection_refs,
                 "proxy_birth_collection_inputs": collection_inputs}
    requests = {
        "member": {"required_input_uses": [claim(tx_hash, consumed)],
                   "required_dependency_uses": [claim(tx_hash, c) for c in (consumed, cluster)]},
        "agent": {"required_dependency_uses": [claim(issued["hash"], proxy)],
                  "required_input_uses": [claim(tx_hash, consumed)]},
        "proxy": {k: [claim(proxy_tx["hash"], c) for c in cells] for k, cells in
                  [("required_input_uses", collection_inputs), ("required_dependency_uses", collection_refs)] if cells},
        "collection": {},
    }
    histories = {}
    for name, required in requests.items():
        cell = locations[name]
        identity = cell["output"]["type"]
        req = {"anchor": header["hash"], "type_hash": hex_bytes(script_hash(identity)),
               "origin": chain.birth(cell)["point"], "end_transaction": cell["point"]["tx_hash"],
               "required_dependencies": [{"data_hash": identity["code_hash"]}], **required}
        path = work / "request.json"
        path.write_text(json.dumps(req))
        proof = json.loads(subprocess.check_output([stem, "ckb-wallet", "prove-type-history",
                           "--request", path, "--l1-rpc", chain.rpc]))
        history = {"request": req, "proof": proof}
        result = verify(stem, work, history, req)
        result.check_returncode()
        history["verified"] = json.loads(result.stdout)
        histories[name] = history
    return {"anchor_height": int(header["number"], 16), "cells": locations, "histories": histories}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc", default="https://testnet.ckb.dev")
    parser.add_argument("--stem", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    chain = Chain(args.rpc)
    cases = []
    with tempfile.TemporaryDirectory() as temp:
        for mint in MINTS:
            cases.append(collect(chain, args.stem.resolve(), Path(temp), mint))
            print("verified", cases[-1]["anchor_height"], flush=True)
    with lzma.open(args.output, "xt") as out:
        json.dump(cases, out, separators=(",", ":"))


if __name__ == "__main__":
    main()

"""Replay the public delegation histories and distinguish spent inputs from resolved CellDeps."""
import argparse
import json
import lzma
from pathlib import Path
import tempfile

from public import AGENT, CLUSTER, HERE, MINTS, PROXY, SPORE, claim, deployed
from replay import binary, ckb_hash, field, hex_bytes, script_hash, verify


def compare_state(cell, state):
    expected = claim("", cell)
    assert state["out_point"] == expected["out_point"]
    assert state["data_hash"] == expected["data_hash"] and state["lock_hash"] == expected["lock_hash"]


def replay(stem, root, case):
    cells = case["cells"]
    verified = {}
    for role, code in [("member", SPORE), ("agent", AGENT), ("proxy", PROXY), ("collection", CLUSTER)]:
        cell, history = cells[role], case["histories"][role]
        result = verify(stem, root, history, history["request"])
        result.check_returncode()
        actual = json.loads(result.stdout)
        assert actual == history["verified"] and deployed(cell, code)
        assert actual["type_hash"] == hex_bytes(script_hash(cell["output"]["type"]))
        assert actual["required_dependencies_per_transition"] == [{"type_hash": None, "data_hash": code}]
        compare_state(cell, actual["transitions"][-1]["output"])
        verified[role] = actual
    assert len({h["anchor"] for h in verified.values()}) == 1
    cluster = cells["collection"]["output"]["type"]["args"]
    member = binary(cells["member"]["data"])
    assert field(member, 2) == (32).to_bytes(4, "little") + binary(cluster)
    assert cells["agent"]["output"]["type"] == cells["spent_agent"]["output"]["type"]
    assert cells["agent"]["output"]["type"]["args"] == cells["proxy"]["data"] == cluster
    assert cells["spent_agent"]["data"] == cells["agent"]["data"] == hex_bytes(script_hash(cells["proxy"]["output"]["type"]))
    birth, = verified["member"]["transitions"]
    mint = birth["transaction"]
    assert mint == verified["agent"]["end_transaction"] and birth["height"] == case["anchor_height"]
    previous, following = verified["agent"]["transitions"][-2:]
    assert following["transaction"] == mint
    compare_state(cells["spent_agent"], previous["output"])
    assert cells["spent_agent"]["output"]["lock"] != cells["collection"]["output"]["lock"]
    agent_use = claim(mint, cells["spent_agent"])
    collection_use = claim(mint, cells["collection"])
    assert verified["member"]["matched_input_uses"] == verified["agent"]["matched_input_uses"] == [agent_use]
    assert verified["member"]["matched_dependency_uses"] == [agent_use, collection_use]
    agent_mint = verified["agent"]["transitions"][0]["transaction"]
    assert verified["agent"]["matched_dependency_uses"] == [claim(agent_mint, cells["proxy"])]
    proxy_mint = verified["proxy"]["transitions"][0]["transaction"]
    for source, result_key in [("inputs", "matched_input_uses"), ("deps", "matched_dependency_uses")]:
        refs = cells["proxy_birth_collection_" + source]
        assert verified["proxy"].get(result_key, []) == [claim(proxy_mint, c) for c in refs]
        for cell in refs:
            spent = next(t["output"] for t in verified["collection"]["transitions"]
                         if t["output"] and t["output"]["out_point"] == cell["point"])
            compare_state(cell, spent)
            assert cell["point"] != cells["collection"]["point"]
            assert cell["output"]["lock"] == cells["collection"]["output"]["lock"]
    assert cells["proxy_birth_collection_deps"] or cells["proxy_birth_collection_inputs"]

    record = case["histories"]["member"]
    req = record["request"]
    owners = owner_inputs(record, collection_use)
    assert owners and all(entry["out_point"] not in (agent_use["out_point"], collection_use["out_point"])
                          for entry in owners)
    with_owners = verify(stem, root, record, {**req, "required_input_uses": [agent_use, *owners]})
    with_owners.check_returncode()
    assert json.loads(with_owners.stdout)["matched_input_uses"] == [agent_use, *owners]
    for uses, msg in [
        ([collection_use], "absent from the transaction's inputs"),
        ([agent_use, agent_use], "duplicate or excessive required input uses"),
        ([{**agent_use, "transaction": proxy_mint}], "required input transaction absent"),
        *[([{**agent_use, k: collection_use[k]}], "resolved CKB input differs")
          for k in ("lock_hash", "type_hash", "data_hash")],
    ]:
        bad = verify(stem, root, record, {**req, "required_input_uses": uses})
        assert bad.returncode != 0 and msg in bad.stderr, (msg, bad.stderr)
    compatible = {k: v for k, v in req.items() if k != "required_input_uses"}
    old = verify(stem, root, record, compatible)
    old.check_returncode()
    assert json.loads(old.stdout) == {k: v for k, v in verified["member"].items() if k != "matched_input_uses"}
    print(case["anchor_height"], {role: len(r["transitions"]) for role, r in verified.items()})
    return mint


def owner_inputs(record, collection_use):
    proof = record["proof"]
    mint = field(binary(proof["steps"][0]["transaction"]), 0)
    tx_hash = hex_bytes(ckb_hash(mint))
    assert tx_hash == collection_use["transaction"]
    parents = {ckb_hash(raw): raw for raw in map(binary, proof["references"])}
    packed = field(mint, 3)
    assert len(packed) == 4 + 44 * int.from_bytes(packed[:4], "little")
    owners = []
    for i in range(4, len(packed), 44):
        point = packed[i+8:i+44]
        parent, index = parents[point[:32]], int.from_bytes(point[32:], "little")
        cell = field(field(parent, 4), index)
        lock = hex_bytes(ckb_hash(field(cell, 1)))
        if lock == collection_use["lock_hash"]:
            data = field(field(parent, 5), index)
            assert int.from_bytes(data[:4], "little") == len(data) - 4
            owners.append({"transaction": tx_hash, "out_point": {"tx_hash": hex_bytes(point[:32]), "index": hex(index)},
                           "lock_hash": lock, "data_hash": hex_bytes(ckb_hash(data[4:]))})
    return owners


def local_mint(stem, work, filename, case):
    with lzma.open(HERE.parent / filename, "rt") as records:
        record = json.load(records)[case]
    collection = record["collection"]
    first = record["verified"]["transitions"][0]["transaction"]
    collection_use, = [u for u in record["verified"]["matched_dependency_uses"] if u["transaction"] == first]
    assert collection_use["type_hash"] == collection["verified"]["type_hash"]
    owners = owner_inputs(record, collection_use)
    assert owners and all(entry["out_point"] != collection_use["out_point"] for entry in owners)
    result = verify(stem, work, record, {**record["request"], "required_input_uses": owners})
    result.check_returncode()
    assert json.loads(result.stdout) == {**record["verified"], "matched_input_uses": owners}
    print(case, "collection owner's inputs spent at mint:", [o["out_point"] for o in owners])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp, lzma.open(HERE / "public.json.xz", "rt") as src:
        stem, work = args.stem.resolve(), Path(tmp)
        assert [replay(stem, work, case) for case in json.load(src)] == list(MINTS)
        local_mint(stem, work, "issuance-deps/settlements.json.xz", "custody")
        local_mint(stem, work, "settlements.json.xz", "import")


if __name__ == "__main__":
    main()

"""Check all published CKB histories with the offline Stem wallet."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3] / "tools"))
from bridge_localnet import binary, ckb_hash, hex_bytes, script, script_hash
from cluster_index import DATA as CLUSTER_DATA_HASH, classify


def check_registrations(settlement, checked):
    registration = settlement["mutant_registration"]
    entries = registration.get("entries", [registration])
    cell = settlement.get("final_ckb") or settlement["final_ckb_cell"]
    data = binary(cell["data"]["content"])
    mime_size = int.from_bytes(data[16:20], "little")
    ids = [binary(entry["type"]["args"])[:32].hex() for entry in entries]
    assert len(set(ids)) == len(ids)
    assert data[20:20 + mime_size] == ("text/plain;mutant[]=" + ",".join(ids)).encode()
    for entry in entries:
        expected = {"type_hash": hex_bytes(script_hash(entry["type"])),
                    "data_hash": hex_bytes(ckb_hash(entry["program"].encode()))}
        assert expected in checked["required_dependencies_per_transition"]
    for step in checked["transitions"]:
        if step["output"] is not None:
            assert step["output"]["data_hash"] == hex_bytes(ckb_hash(data))


def verify(stem, root, record, request):
    req, proof = root / "request.json", root / "proof.json"
    req.write_text(json.dumps(request))
    proof.write_text(json.dumps(record["proof"]))
    return subprocess.run([stem, "ckb-wallet", "verify-type-history", "--request", req,
                           "--proof", proof], text=True, capture_output=True)


def field(raw, index):
    # The Rust wallet has already validated these canonical Molecule bytes.
    header = int.from_bytes(raw[4:8], "little")
    pos = 4 + 4 * index
    assert pos < header
    start = int.from_bytes(raw[pos:pos + 4], "little")
    end = int.from_bytes(raw[pos + 4:pos + 8], "little") if pos + 4 < header else len(raw)
    return raw[start:end]


def direct_dependencies(step):
    packed = field(field(binary(step["transaction"]), 0), 1)
    count = int.from_bytes(packed[:4], "little")
    assert len(packed) == 4 + count * 37
    return {(hex_bytes(packed[i:i + 32]), int.from_bytes(packed[i + 32:i + 36], "little"))
            for i in range(4, len(packed), 37) if packed[i + 36] == 0}


def check_member_history(stem, root, record):
    settlement = record["settlement"]
    collection = settlement["cluster"]
    history = record["collection"]
    result = verify(stem, root, history, history["request"])
    result.check_returncode()
    verified = json.loads(result.stdout)
    member = record["verified"]
    assert verified == history["verified"] and verified["anchor"] == member["anchor"]
    assert verified["type_hash"] == hex_bytes(script_hash(collection["type"]))
    assert collection["type"]["args"] == collection["id"]
    assert collection["type"]["hash_type"] == "data1" and collection["type"]["code_hash"] == CLUSTER_DATA_HASH
    program = {"type_hash": None, "data_hash": CLUSTER_DATA_HASH}
    assert program in verified["required_dependencies_per_transition"]
    assert program in member["required_dependencies_per_transition"]
    assert {"type_hash": verified["type_hash"], "data_hash": collection["data_hash"]} in member["required_dependencies_per_transition"]
    cell = settlement.get("final_ckb") or settlement["final_ckb_cell"]
    data = binary(cell["data"]["content"])
    offset = int.from_bytes(data[12:16], "little")
    assert data[offset:] == (32).to_bytes(4, "little") + binary(collection["id"])
    steps = verified["transitions"]
    assert all(step["output"]["data_hash"] == collection["data_hash"] for step in steps)
    assert steps[0]["transaction"] == collection["minted"]["hash"] and steps[0]["height"] < member["transitions"][0]["height"]
    if "transferred" in collection:
        change = steps[1]
        assert len(steps) == 2 and change["transaction"] == collection["transferred"]["hash"]
        assert change["output"]["lock_hash"] != steps[0]["output"]["lock_hash"]
        release = next(step for step in member["transitions"] if step["transaction"] == settlement["released"]["hash"])
        assert member["transitions"][1]["height"] < change["height"] < release["height"]
    else:
        assert len(steps) == 1 and settlement["native_content"] == hex_bytes(data)
    for tx in record["proof"]["steps"]:
        state = max((step for step in steps if step["height"] <= tx["height"]), key=lambda step: step["height"])
        point = state["output"]["out_point"]
        assert (point["tx_hash"], int(point["index"], 16)) in direct_dependencies(tx)
    print("  collection", len(steps), "CKB transitions at the member anchor")


def check_handoffs(record):
    settlement = record["settlement"]
    terms = settlement["handoffs"]
    issued = settlement.get("origin") == "stem"
    original_id = script_hash(settlement["native_type"] if issued else settlement["original_type"])
    spore = script_hash(settlement["spore_type"]) if issued else original_id
    original_data = binary(settlement["native_content"] if issued else settlement["final_ckb"]["data"]["content"])
    mailbox = settlement["mailbox"]
    policy = binary(mailbox["args"])
    endpoint = script_hash(settlement["endpoint"])
    previous_native = settlement["restored"] if issued else settlement["imported"]
    verified = record["verified"]["transitions"]
    changes = terms + ([settlement["stale_handoff"]] if "stale_handoff" in settlement else [])
    for handoff in changes:
        pos = next(j for j, tx in enumerate(verified) if tx["transaction"] == handoff["ckb"]["hash"])
        raw_tx = field(binary(record["proof"]["steps"][pos]["transaction"]), 0)
        mailbox_data = field(field(raw_tx, 5), 0)
        assert int.from_bytes(mailbox_data[:4], "little") == len(mailbox_data) - 4
        message = mailbox_data[44:]
        assert message == binary(handoff["message"]["record"])
        assert message[:128] == policy[32:96] + script_hash(mailbox) + endpoint
        assert int.from_bytes(message[128:136], "little") + 1 == int.from_bytes(mailbox_data[4:12], "little")
        payload = message[136:]
        assert len(payload) == 204 and payload[:32] == original_id and payload[64:68] == b"SPT1"
        for lock, owner, state in [(handoff["previous_custody"], payload[68:100], verified[pos-1]),
                                   (handoff["next_custody"], payload[32:64], verified[pos])]:
            args = binary(lock["args"])
            assert lock["hash_type"] == "data2" and len(args) == 117 and args[0] == 5
            assert args[1:65] == policy[96:128] + spore
            assert state["output"]["lock_hash"] == hex_bytes(script_hash(lock))
            assert owner == script_hash(script(hex_bytes(args[65:97]), hex_bytes(args[97:]), "data"))
        source = handoff["source"]
        assert payload[100:136] == binary(source["tx_hash"]) + int(source["index"], 16).to_bytes(4, "little")
        assert payload[136:168] == ckb_hash(binary(handoff["successor"]))
        assert int.from_bytes(payload[168:172], "little") == len(original_data)
        assert payload[172:] == ckb_hash(original_data)
        expected = {"tx_hash": "0x" + previous_native["hash"], "index": "0x1"}
        if "native" in handoff:
            assert expected == source
            previous_native = handoff["native"]
        else:
            assert expected != source and expected == handoff["unspent_native_source"]
            assert "spent_handoff_source" in settlement["rejections"]
        assert len({payload[32:64], payload[68:100]}) == 2
    print("  verified", len(terms), "CKB authorizations for direct native spends")


def check_public_collection(stem, root):
    with lzma.open(HERE / "public-collection.json.xz", "rt") as source:
        record = json.load(source)
    with lzma.open(HERE / "cluster-index-history.json.xz", "rt") as source:
        inventory = json.load(source)
    result = verify(stem, root, record, record["request"])
    result.check_returncode()
    checked = json.loads(result.stdout)
    assert checked == record["verified"] and checked["type_hash"] == hex_bytes(script_hash(record["type"]))
    data = binary(record["data"])
    assert classify(data) == ("absent", None)
    rows = {tuple(row["out_point"]): row for row in inventory["datasets"][0]["rows"]}
    first, second = checked["transitions"]
    assert first["output"]["lock_hash"] != second["output"]["lock_hash"]
    for step in (first, second):
        cell = step["output"]
        p = cell["out_point"]
        row = rows[p["tx_hash"], int(p["index"], 16)]
        assert row["args"] == record["type"]["args"] and row["height"] == step["height"]
        assert row["shape"] == "absent" and row["data_hash"] == cell["data_hash"] == hex_bytes(ckb_hash(data))
    print("Public testnet collection verified across two owners")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    records = {}
    for filename in ("histories.json.xz", "two-registrations.json.xz", "cluster-controls.json.xz", "cluster-settlement.json.xz",
                     "handoff-compact.json.xz", "handoff-cluster.json.xz", "handoff-large.json.xz",
                     "handoff-import-compact.json.xz", "handoff-import-cluster.json.xz", "handoff-import-large.json.xz",
                     "registered-policy/settlement.json.xz"):
        with lzma.open(HERE / filename, "rt") as source:
            records.update(json.load(source))
    with tempfile.TemporaryDirectory() as scratch:
        root = Path(scratch)
        for name, record in records.items():
            result = verify(args.stem.resolve(), root, record, record["request"])
            result.check_returncode()
            checked = json.loads(result.stdout)
            assert checked == record["verified"], name
            if "settlement" in record:
                assert len({p["member"] for p in record["replicas"]}) == 4
                assert all(peer["height"] == record["agreed"]["height"] and
                           peer["app_hash"] == record["agreed"]["app_hash"] for peer in record["replicas"])
                settlement = record["settlement"]
                check_registrations(settlement, checked)
                assert "local_mutant_transfer" in settlement["rejections"]
                assert checked["ended_in_burn"] == bool(settlement.get("standard_melt"))
                if "collection" in record:
                    check_member_history(args.stem.resolve(), root, record)
                if "handoffs" in settlement:
                    check_handoffs(record)
            print(name, len(checked["transitions"]), "CKB transitions")
        collection, member = records["collection"], records["cluster-member"]
        cluster_data = binary(collection["operations"]["mint"]["signed"]["json"]["outputs_data"][0])
        member_data = binary(member["operations"]["mint"]["signed"]["json"]["outputs_data"][0])
        cluster_offset = int.from_bytes(member_data[12:16], "little")
        assert member_data[cluster_offset:] == (32).to_bytes(4, "little") + binary(collection["type"]["args"])
        assert hex_bytes(script_hash(collection["type"])) == collection["verified"]["type_hash"]
        assert {"type_hash": collection["verified"]["type_hash"], "data_hash": hex_bytes(ckb_hash(cluster_data))} in (
            member["verified"]["required_dependencies_per_transition"])
        for record, data in [(member, member_data), (collection, cluster_data)]:
            surviving = [s["output"] for s in record["verified"]["transitions"] if s["output"] is not None]
            assert surviving and all(cell["data_hash"] == hex_bytes(ckb_hash(data)) for cell in surviving)
        # Same real proof, stronger caller predicate. The corpus contains both
        # the program text and executable bytes; only their authentic Types match.
        asset = records["mutant"]
        original = asset["request"]["required_dependencies"]
        registry = original[-1]
        imported = records["cluster-import"]
        other_collection = records["cluster-export"]["collection"]["verified"]["type_hash"]
        member_deps = imported["request"]["required_dependencies"]
        assert member_deps[-1]["data_hash"] == records["cluster-export"]["request"]["required_dependencies"][-1]["data_hash"]
        assert member_deps[-1]["type_hash"] != other_collection
        for record, required in [
            (records["plain"], original),
            (asset, [{**registry, "data_hash": original[0]["data_hash"]}]),
            (asset, [{**registry, "type_hash": asset["request"]["type_hash"]}]),
            (imported, [*member_deps[:-1], {**member_deps[-1], "type_hash": other_collection}]),
        ]:
            result = verify(args.stem.resolve(), root, record, {**record["request"], "required_dependencies": required})
            assert result.returncode and "required CKB dependency absent" in result.stderr
        print("Four unmatched dependency predicates declined")
        check_public_collection(args.stem.resolve(), root)


if __name__ == "__main__":
    main()

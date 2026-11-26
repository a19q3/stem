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
from bridge_localnet import binary, ckb_hash, hex_bytes, script_hash
from cluster_index import DATA as CLUSTER_DATA_HASH, classify


def check_registrations(settlement, checked):
    registration = settlement["mutant_registration"]
    entries = registration.get("entries", [registration])
    cell = settlement.get("final_ckb") or settlement["final_ckb_cell"]
    data = binary(cell["data"]["content"])
    mime_size = int.from_bytes(data[16:20], "little")
    ids = [entry["type"]["args"][2:] for entry in entries]
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


def direct_dependencies(step):
    # The Rust wallet has already validated the canonical CKB Transaction bytes.
    def field(raw, index):
        offsets = [int.from_bytes(raw[pos:pos + 4], "little") for pos in (4 + 4 * index, 8 + 4 * index)]
        return raw[offsets[0]:offsets[1]]
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
        assert member["transitions"][1]["height"] < change["height"] < member["transitions"][2]["height"]
    else:
        assert len(steps) == 1 and settlement["native_content"] == hex_bytes(data)
    for tx in record["proof"]["steps"]:
        state = max((step for step in steps if step["height"] <= tx["height"]), key=lambda step: step["height"])
        point = state["output"]["out_point"]
        assert (point["tx_hash"], int(point["index"], 16)) in direct_dependencies(tx)
    print("  collection", len(steps), "CKB transitions at the member anchor")


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
    for filename in ("histories.json.xz", "two-registrations.json.xz", "cluster-controls.json.xz", "cluster-settlement.json.xz"):
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

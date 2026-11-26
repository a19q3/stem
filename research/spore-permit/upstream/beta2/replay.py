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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    records = {}
    for filename in ("histories.json.xz", "two-registrations.json.xz", "cluster-controls.json.xz"):
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
        for record, required in [
            (records["plain"], original),
            (asset, [{**registry, "data_hash": original[0]["data_hash"]}]),
            (asset, [{**registry, "type_hash": asset["request"]["type_hash"]}]),
        ]:
            result = verify(args.stem.resolve(), root, record, {**record["request"], "required_dependencies": required})
            assert result.returncode and "required CKB dependency absent" in result.stderr
        print("Three unmatched dependency predicates declined")


if __name__ == "__main__":
    main()

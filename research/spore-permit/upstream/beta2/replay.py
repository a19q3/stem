"""Check all published CKB histories with the offline Stem wallet."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent


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
    with lzma.open(HERE / "histories.json.xz", "rt") as source:
        records = json.load(source)
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
                assert "local_mutant_transfer" in settlement["rejections"]
                assert checked["ended_in_burn"] == bool(settlement.get("standard_melt"))
            print(name, len(checked["transitions"]), "CKB transitions")
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

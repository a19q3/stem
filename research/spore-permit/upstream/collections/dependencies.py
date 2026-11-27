"""Compare authenticated member transactions before and after narrowing ordinary CellDeps."""
import argparse
import json
import lzma
from pathlib import Path
import tempfile

from check import archive, checked, check_collection, public_metadata
from migrations import HERE
from replay import binary, check_handoffs, direct_dependencies, field, hex_bytes, script_hash


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", required=True, type=Path)
    args = parser.parse_args()
    stem = args.stem.resolve()
    original = archive("settlements.json.xz")
    with lzma.open(HERE / "issuance-deps" / "settlements.json.xz", "rt") as source:
        candidate = json.load(source)
    assert set(candidate) == {"ordinary", "custody", "immortal"}
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        public = public_metadata(stem, work)
        for name, record in candidate.items():
            baseline = original[name]
            before, after = [checked(stem, work, r) for r in (baseline, record)]
            settlement = record["settlement"]
            assert len({peer["member"] for peer in record["replicas"]}) == 4
            assert all((p["height"], p["app_hash"]) == (record["agreed"]["height"], record["agreed"]["app_hash"])
                       for p in record["replicas"])
            assert before["ended_in_burn"] == after["ended_in_burn"] == (name != "immortal")
            member = settlement["final_ckb_cell"]
            assert member["data"]["content"] == settlement["native_content"]
            assert settlement["spore_type"] == member["output"]["type"] == settlement["original_ckb_cell"]["output"]["type"]
            assert after["type_hash"] == hex_bytes(script_hash(settlement["spore_type"]))
            check_collection(stem, work, name, record, after, member, public, mint_only=True)
            assert after["required_dependencies_per_transition"] == [
                {"type_hash": None, "data_hash": settlement["spore_type"]["code_hash"]}]
            if name in ("immortal", "custody"):
                assert len(settlement["handoffs"]) == 2
                check_handoffs(record)
            point = settlement["cluster"]["minted"]["hash"], 0
            prior_point = baseline["settlement"]["cluster"]["minted"]["hash"], 0
            removed = []
            transaction_delta = []
            for i, (old, new) in enumerate(zip(baseline["proof"]["steps"], record["proof"]["steps"], strict=True)):
                assert prior_point in direct_dependencies(old)
                assert (point in direct_dependencies(new)) == (i == 0)
                tx1, tx2 = binary(old["transaction"]), binary(new["transaction"])
                raw1, raw2 = field(tx1, 0), field(tx2, 0)
                deps1, deps2 = field(raw1, 1), field(raw2, 1)
                saved = len(deps1) - len(deps2)
                assert saved == (37 if i == 0 else 74) and len(raw1) - len(raw2) == saved
                removed.append(saved)
                transaction_delta.append(len(tx1) - len(tx2))
            print(name, "raw bytes saved by step", removed, "witnessed transaction deltas", transaction_delta,
                  "common Stem height", record["agreed"]["height"])


if __name__ == "__main__":
    main()

"""Replay the public births and both directions of ordinary Cluster settlement offline."""
import argparse
from collections import Counter
import json
import lzma
from pathlib import Path
import tempfile

from migrations import DATA, HERE, TESTNET_TYPE
from pack import CASES
from cluster_index import classify
from replay import (binary, check_handoffs, check_registrations, ckb_hash, field, hex_bytes,
                    script_hash, verify)


def archive(name):
    with lzma.open(HERE / name, "rt") as source:
        return json.load(source)


def checked(stem, root, record):
    result = verify(stem, root, record, record["request"])
    result.check_returncode()
    actual = json.loads(result.stdout)
    assert actual == record["verified"]
    return actual


def public_metadata(stem, root):
    extended = {}
    for chain, count, ids, extra in [("mainnet", 1853, 492, 0), ("testnet", 776, 487, 3)]:
        index = archive(chain + ".json.xz")
        data, *typed = index["datasets"]
        rows = data["rows"]
        assert len(rows) == len({tuple(r["out_point"]) for r in rows}) == count
        assert len({r["args"] for r in rows}) == ids
        assert data["query"]["script"] == {"code_hash": DATA, "hash_type": "data1", "args": "0x"}
        assert all(r["height"] <= int(index["tip"]["block_number"], 16) for r in rows)
        assert Counter(r["shape"] for r in rows) == Counter({"absent": count - extra, "extended-absent": extra})
        if chain == "testnet":
            assert len(typed) == 1 and not typed[0]["rows"] and typed[0]["query"]["script"]["code_hash"] == TESTNET_TYPE
            extended = {tuple(r["out_point"]): r for r in rows if r["shape"] == "extended-absent"}
        else:
            assert not typed
    samples = set()
    for record in archive("four-field-histories.json.xz"):
        actual = checked(stem, root, record)
        step, = actual["transitions"]
        point = step["output"]["out_point"]
        row = extended.pop((point["tx_hash"], int(point["index"], 16)))
        raw = binary(record["data"])
        assert actual["type_hash"] == hex_bytes(script_hash(record["type"]))
        assert record["type"] == {"code_hash": DATA, "hash_type": "data1", "args": row["args"]}
        assert step["height"] == row["height"] and len(raw) == row["size"]
        assert step["output"]["data_hash"] == row["data_hash"] == hex_bytes(ckb_hash(raw))
        assert classify(raw) == ("extended-absent", None) and int.from_bytes(raw[4:8], "little") == 20
        trailer = field(raw, 3)
        assert field(raw, 2) == b"" and int.from_bytes(trailer[:4], "little") == len(trailer) - 4
        assert trailer[4:].startswith(b"ipfs://")
        samples.add(raw)
    assert not extended and len(samples) == 2
    return min(samples, key=len)


def check_collection(stem, root, name, record, member, cell, public, mint_only=False):
    settlement = record["settlement"]
    cluster = settlement["cluster"]
    history = checked(stem, root, record["collection"])
    member_data = binary(cell["data"]["content"])
    assert cluster["id"] == cluster["type"]["args"]
    assert field(member_data, 2) == (32).to_bytes(4, "little") + binary(cluster["id"])
    assert history["type_hash"] == hex_bytes(script_hash(cluster["type"])) and history["anchor"] == member["anchor"]
    steps = history["transitions"]
    assert steps[0]["transaction"] == cluster["minted"]["hash"]
    assert all(s["output"]["data_hash"] == cluster["data_hash"] for s in steps)
    assert len(steps) == (2 if name == "import" else 1)
    if name == "import":
        assert steps[1]["transaction"] == cluster["transferred"]["hash"]
        assert steps[0]["output"]["lock_hash"] != steps[1]["output"]["lock_hash"]
        assert member["transitions"][1]["height"] < steps[1]["height"] < member["transitions"][2]["height"]
    if name != "registered":
        assert cluster["type"]["code_hash"] == DATA
    if name in ("ordinary", "custody", "import"):
        assert cluster["data_hash"] == hex_bytes(ckb_hash(public))
    events = member["transitions"][:1] if mint_only or name == "import" else member["transitions"]
    expected = []
    for event in events:
        state = max((s for s in steps if s["height"] <= event["height"]), key=lambda s: s["height"])
        output = state["output"]
        expected.append({"transaction": event["transaction"], "type_hash": history["type_hash"],
                         **{k: output[k] for k in ("data_hash", "lock_hash", "out_point")}})
    assert member["matched_dependency_uses"] == expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", required=True, type=Path)
    args = parser.parse_args()
    stem = args.stem.resolve()
    with tempfile.TemporaryDirectory() as work:
        root = Path(work)
        public = public_metadata(stem, root)
        print("Three public births authenticated")
        records = archive("settlements.json.xz")
        assert set(records) == set(CASES)
        for name, record in records.items():
            s = record["settlement"]
            verified = checked(stem, root, record)
            assert len({p["member"] for p in record["replicas"]}) == 4
            assert all((p["app_hash"], p["height"]) == (record["agreed"]["app_hash"], record["agreed"]["height"])
                       for p in record["replicas"])
            cell = s.get("final_ckb_cell") or s["final_ckb"]
            assert verified["type_hash"] == hex_bytes(script_hash(cell["output"]["type"]))
            assert verified["ended_in_burn"] == (name in {"plain", "ordinary", "custody"})
            assert all(t["output"]["data_hash"] == hex_bytes(ckb_hash(binary(cell["data"]["content"])))
                       for t in verified["transitions"] if t["output"])
            if name != "import":
                assert s["native_content"] == cell["data"]["content"]
                assert s["original_ckb_cell"]["output"]["type"] == cell["output"]["type"] == s["spore_type"]
                assert bool((s.get("standard_melt") or {}).get("retired")) == (name in {"plain", "custody"})
            if name != "plain":
                check_collection(stem, root, name, record, verified, cell, public)
            if name in ("ordinary", "custody", "immortal"):
                assert binary(s["native_type"]["args"])[0] == binary(s["endpoint"]["args"])[0] == 6
                assert "local_cluster_transfer" in s["rejections"]
            if name == "registered":
                assert binary(s["native_type"]["args"])[0] == 4 and binary(s["endpoint"]["args"])[0] == 3
                check_registrations(s, verified)
            if name in ("registered", "custody", "immortal"):
                assert len(s["handoffs"]) == 2
                check_handoffs(record)
            print(name, len(verified["transitions"]), "member transitions, four replicas at", record["agreed"]["height"])


if __name__ == "__main__":
    main()

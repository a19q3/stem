"""Replay exact historical CellDep claims against authenticated Spore transitions."""
import argparse
import json
import lzma
from pathlib import Path
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from replay import binary, ckb_hash, direct_dependencies, field, hex_bytes, verify


def read(path):
    with lzma.open(path, "rt") as archive:
        return json.load(archive)


def cell_use(tx_hash, output, type_hash):
    return {"transaction": tx_hash, "type_hash": type_hash,
            **{k: output[k] for k in ("out_point", "lock_hash", "data_hash")}}


def check(stem, root, record, uses, error=None):
    request = {**record["request"], "required_dependency_uses": uses}
    result = verify(stem, root, record, request)
    if error is not None:
        assert result.returncode and error in result.stderr, result.stderr
        return
    result.check_returncode()
    verified = json.loads(result.stdout)
    expected = {k: v for k, v in record["verified"].items() if k != "matched_dependency_uses"}
    if uses:
        expected["matched_dependency_uses"] = uses
    assert verified == expected


def dep_group_mint(record):
    mint = record["proof"]["steps"][0]
    raw = field(binary(mint["transaction"]), 0)
    tx_hash = hex_bytes(ckb_hash(raw))
    parents = {ckb_hash(tx): tx for tx in map(binary, record["proof"]["references"])}
    packed = field(raw, 1)
    direct = direct_dependencies(mint)

    def unpack(point):
        return {"tx_hash": hex_bytes(point[:32]), "index": hex(int.from_bytes(point[32:], "little"))}

    def resolve(point):
        index = int.from_bytes(point[32:], "little")
        parent = parents[point[:32]]
        cell = field(field(parent, 4), index)
        data = field(field(parent, 5), index)[4:]
        use = {"transaction": tx_hash, "out_point": unpack(point), "data_hash": hex_bytes(ckb_hash(data)),
               "lock_hash": hex_bytes(ckb_hash(field(cell, 1)))}
        if field(cell, 2):
            use["type_hash"] = hex_bytes(ckb_hash(field(cell, 2)))
        return use, data

    containers = []
    expanded = set()
    for i in range(4, len(packed), 37):
        if packed[i+36] == 1:
            group, data = resolve(packed[i:i+36])
            containers.append(group)
            assert len(data) == 4 + 36 * int.from_bytes(data[:4], "little")
            expanded.update(data[j:j+36] for j in range(4, len(data), 36))
    members = [resolve(p)[0] for p in sorted(expanded)
               if (hex_bytes(p[:32]), int.from_bytes(p[32:], "little")) not in direct]
    assert containers and members
    expanded_points = [unpack(p) for p in expanded]
    container = next(g for g in containers if g["out_point"] not in expanded_points and
                     (g["out_point"]["tx_hash"], int(g["out_point"]["index"], 16)) not in direct)
    return members, container


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    stem = parser.parse_args().stem.resolve()
    absent = "required OutPoint absent"
    mismatch = "resolved CKB dependency differs"
    public = read(HERE.parent / "registered-policy/public.json.xz")
    with tempfile.TemporaryDirectory() as folder:
        root = Path(folder)
        registration, member = public["registration"], public["member"]
        check(stem, root, registration, [])
        states = registration["verified"]["transitions"]
        mint, = member["verified"]["transitions"]
        assert registration["request"]["anchor"] == member["request"]["anchor"]
        assert states[0]["height"] < states[1]["height"] < mint["height"]
        before, after = [cell_use(mint["transaction"], state["output"], registration["request"]["type_hash"])
                         for state in states]
        assert before["data_hash"] == after["data_hash"] and before["lock_hash"] != after["lock_hash"]
        check(stem, root, member, [after])
        check(stem, root, member, [before], absent)
        check(stem, root, member, [{**after, "lock_hash": before["lock_hash"]}], mismatch)
        print("Public testnet mint at", mint["height"], "referenced registration", after["out_point"])

        for name, record in read(HERE / "settlement.json.xz").items():
            uses = record["request"]["required_dependency_uses"]
            check(stem, root, record["collection"], [])
            states = record["collection"]["verified"]["transitions"]
            steps = record["verified"]["transitions"]
            assert record["collection"]["request"]["anchor"] == record["request"]["anchor"]
            for step, actual in zip(steps, uses, strict=True):
                state = max((s for s in states if s["height"] <= step["height"]), key=lambda s: s["height"])
                assert actual == cell_use(step["transaction"], state["output"], record["collection"]["request"]["type_hash"])
            check(stem, root, record, [])
            check(stem, root, record, uses)
            members, wrapper = dep_group_mint(record)
            check(stem, root, record, [*uses, *members])
            check(stem, root, record, [wrapper], absent)
            check(stem, root, record, [uses[0], uses[0]], "duplicate or excessive required dependency uses")
            check(stem, root, record, [{**uses[0], "transaction": states[0]["transaction"]}],
                  "required dependency transaction absent")
            check(stem, root, record, [{**uses[0], "type_hash": record["request"]["type_hash"]}], mismatch)
            check(stem, root, record, [{**uses[0], "data_hash": record["request"]["type_hash"]}], mismatch)
            if len(states) == 2:
                assert uses[0]["out_point"] != uses[-1]["out_point"]
                check(stem, root, record, [{**uses[0], "transaction": uses[-1]["transaction"]}], absent)
            print(name, len(uses), "Cluster dependency uses,", len(members), "DepGroup-only mint dependencies")


if __name__ == "__main__":
    main()

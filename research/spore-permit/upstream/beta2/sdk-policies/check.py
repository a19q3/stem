"""Check SDK program identities, independent net mint payments and actual changes of holder."""
import argparse
import json
import lzma
from pathlib import Path
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "registered-policy"))
from verify import PROGRAM, authenticated, binary, ckb_hash, direct_dependencies, field, hex_bytes, mint_payment

TRANSFER = "0xe2d6c66e5a7828d9b95d3d88fd24d0fa8cf241f34eeb141cdeb01f00536b0179"
FIRST_OUTPUT = "0x26fbe71ea4da9ef403d2c3a8dd91cf117d6adb6a014258f7c928f7cb5b058701"


def read(name):
    with lzma.open(HERE / name, "rt") as archive:
        return json.load(archive)


def check_layout_limit(stem, root, archive, expected_index, amounts):
    record = read(archive)
    checked = authenticated(stem, root, record)
    types, _, credits, _ = mint_payment(record, checked, program_hashes=(FIRST_OUTPUT, TRANSFER), separate_payee=True)
    assert len(types) == 2 and sorted(credits.values()) == amounts
    mint, = checked["transitions"]
    assert int(checked["origin"]["index"], 16) == 0
    assert direct_dependencies(record["proof"]["steps"][0]) <= direct_dependencies({"transaction": record["deposit_molecule"]})
    body = field(binary(record["deposit_molecule"]), 0)
    assert hex_bytes(ckb_hash(body)) == record["deposit_hash"]
    inputs, outputs = field(body, 3), field(body, 4)
    point = binary(checked["origin"]["tx_hash"]) + bytes(4)
    assert point in [inputs[i+8:i+44] for i in range(4, len(inputs), 44)]
    count = int.from_bytes(outputs[4:8], "little") // 4 - 1
    index, = [i for i in range(count) if hex_bytes(ckb_hash(field(field(outputs, i), 2))) == checked["type_hash"]]
    assert index == expected_index and hex_bytes(ckb_hash(field(field(body, 5), index)[4:])) == mint["output"]["data_hash"]
    assert "error code 89" in record["estimate_error"]
    print(archive, "authenticated mint at output zero, recorded deposit estimate returned 89, successor index", index)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    records = read("settlement.json.xz")
    names = {f"sdk-transfer-{payee}-{origin}" for payee in ("shared", "distinct") for origin in ("import", "native")}
    assert names == records.keys()
    with tempfile.TemporaryDirectory() as scratch:
        root = Path(scratch)
        stem = args.stem.resolve()
        check_layout_limit(stem, root, "first-output.json.xz", 1, [2000])
        check_layout_limit(stem, root, "first-output-zero.json.xz", 0, [1000, 1000])
        for name, record in records.items():
            checked = authenticated(stem, root, record)
            types, _, credits, shape = mint_payment(record, checked, program_hashes=(PROGRAM, TRANSFER), separate_payee=True)
            assert len(types) == 2 and sorted(credits.values()) == ([1000, 1000] if "distinct" in name else [2000])
            locks = [s["output"]["lock_hash"] for s in checked["transitions"]]
            assert len(locks) == (7 if name.endswith("import") else 6) and all(x != y for x, y in zip(locks, locks[1:]))
            settlement = record["settlement"]
            assert len(settlement["handoffs"]) == 2 and shape[-1] == settlement["minted"]["bytes"]
            print(name, "credit", sorted(credits.values()), "inputs/outputs/fee/body/signed", shape,
                  "mint cycles", settlement["minted"]["cycles"], "CKB states", len(locks),
                  "native executions", len(settlement["execution"]))


if __name__ == "__main__":
    main()

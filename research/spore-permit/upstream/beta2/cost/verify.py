"""Verify the CKB transactions used in the custody cost comparison and their native finality anchors."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from replay import binary, ckb_hash, direct_dependencies, field, hex_bytes, verify


def verify_case(stem, root, case, gate):
    result = verify(stem, root, case, case["request"])
    result.check_returncode()
    history = json.loads(result.stdout)
    parents = {hex_bytes(ckb_hash(raw)): raw for raw in map(binary, case["proof"]["references"])}
    steps = {tx["transaction"]: raw for tx, raw in zip(
        history["transitions"], case["proof"]["steps"], strict=True)}
    cycles = []
    for label, measurement in case["costs"].items():
        step = steps[measurement["hash"]]
        packed = binary(step["transaction"])
        assert len(packed) == measurement["bytes"]
        if label in ("grant_a", "grant_b"):
            assert any(hex_bytes(ckb_hash(field(field(parents[tx], 5), index)[4:])) == gate
                       for tx, index in direct_dependencies(step) if tx in parents)
            raw = field(packed, 0)
            holder = field(field(field(raw, 4), 1), 1)
            assert hex_bytes(field(holder, 0)) == gate
            assert field(holder, 2)[4] == 5
            original = field(field(raw, 5), 1)[4:]
            assert len(field(original, 1)) - 4 == case["content_bytes"]
            cycles.append(measurement["cycles"])
    assert len(cycles) == 2
    for key in ("changed_content_byte", "shortened_content"):
        if key in case["rejections"]:
            error = case["rejections"][key]
            assert gate[2:] in error and "error code 112" in error
    assert len({r["member"] for r in case["replicas"]}) == 4
    # Status is sampled after the harness compares all four Headers at the common height.
    tips = {case["common"]["height"]: case["common"]["app_hash"]}
    for replica in case["replicas"]:
        height, app_hash = replica["height"], replica["app_hash"]
        assert height >= case["common"]["height"] and tips.setdefault(height, app_hash) == app_hash
    genesis, proof = root / "genesis.json", root / "native.json"
    genesis.write_text(json.dumps(case["genesis"]))
    proof.write_text(json.dumps(case["finality"]))
    checked = json.loads(subprocess.check_output([stem, "jolteon-proof", "--genesis", genesis, "--proof", proof]))
    assert checked["height"] == case["native_height"] <= case["common"]["height"]
    return cycles


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    with lzma.open(HERE / "cases.json.xz", "rt") as stream:
        archive = json.load(stream)
    with tempfile.TemporaryDirectory() as scratch:
        for name, case in archive["cases"].items():
            gate = archive["programs"][name.split("-")[0]]["data_hash"]
            cycles = verify_case(args.stem.resolve(), Path(scratch), case, gate)
            print(name, *cycles, f"mean {sum(cycles)/2:,.1f}")


if __name__ == "__main__":
    main()

"""Verify the archived CKB histories offline and check that altered histories and predicates fail."""

import argparse
from copy import deepcopy
import json
import lzma
from pathlib import Path
import subprocess
import tempfile


def raw_transaction(packed):
    data = bytes.fromhex(packed.removeprefix("0x"))
    raw, witnesses = (int.from_bytes(data[i:i+4], "little") for i in (4, 8))
    return "0x" + data[raw:witnesses].hex()


def check(stem, directory, request, proof, expected_error=None):
    req = directory / "request.json"
    bundle = directory / "proof.json"
    req.write_text(json.dumps(request))
    bundle.write_text(json.dumps(proof))
    result = subprocess.run([stem, "ckb-wallet", "verify-type-history", "--request", req, "--proof", bundle],
                            text=True, capture_output=True)
    if expected_error is None:
        result.check_returncode()
        return json.loads(result.stdout)
    assert result.returncode and expected_error in result.stderr, (expected_error, result.stderr)
    return result.stderr.strip()


def controls(case):
    def change_proof():
        return deepcopy(case["proof"])
    request, proof = case["request"], case["proof"]
    wrong_hash = "0x" + "01" * 32
    for field, message in [("anchor", "wrong trusted CKB anchor"), ("type_hash", "wrong CKB Type birth"),
                           ("end_transaction", "wrong final transaction")]:
        yield field, dict(request, **{field: wrong_hash}), proof, message
    yield "wrong birth output", dict(request, origin=dict(request["origin"], index="0x3")), proof, "wrong CKB Type birth"
    yield "reversed requirements", dict(request, required_states=request["required_states"][::-1]), proof, "absent or out of order"
    yield "same visit twice", dict(request, required_states=[request["required_states"][0]] * 2), proof, "absent or out of order"
    yield "empty predicate", dict(request, required_states=[{}]), proof, "invalid intermediate state requirements"
    for i, name in [(0, "birth only"), (-1, "end only")]:
        data = case["verified"]["transitions"][i]["output"]["data_hash"]
        yield name, dict(request, required_states=[{"data_hash": data}]), proof, "absent or out of order"
    p = change_proof()
    del p["headers"][1]
    yield "skipped Header", request, p, "disconnected CKB Header chain"
    p = change_proof()
    removed = p["steps"].pop(1)
    p["references"].append(raw_transaction(removed["transaction"]))
    yield "hidden transition with authentic reference", dict(request, required_states=[]), p, "disconnected CKB Type history"
    p = change_proof()
    wire = bytearray.fromhex(p["steps"][1]["transaction"][2:])
    wire[-1] ^= 1
    p["steps"][1]["transaction"] = "0x" + wire.hex()
    yield "changed witness", request, p, "proof does not match the Header"
    p = change_proof()
    p["steps"][1]["proof"] = p["steps"][2]["proof"]
    yield "swapped inclusion", request, p, "outside the trusted block"
    p = change_proof()
    p["references"].pop()
    yield "missing reference", request, p, "missing referenced CKB transaction"
    p = change_proof()
    p["references"].append(p["references"][0])
    yield "duplicated reference", request, p, "duplicate Type history reference"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", required=True, type=Path)
    parser.add_argument("--archive", type=Path, default=Path(__file__).with_name("histories.json.xz"))
    args = parser.parse_args()
    with lzma.open(args.archive, "rt") as stream:
        cases = json.load(stream)
    results = {}
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        for name, case in cases.items():
            actual = check(args.stem.resolve(), directory, case["request"], case["proof"])
            assert actual == case["verified"], name
            results[name] = {"transitions": len(actual["transitions"]), "matched":len(actual["matched_states"]),
                             "burned": actual["ended_in_burn"]}
        for name, request, proof, error in controls(cases["mailbox"]):
            results[name] = check(args.stem.resolve(), directory, request, proof, error)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()

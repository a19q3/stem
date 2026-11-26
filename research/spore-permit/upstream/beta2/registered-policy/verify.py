"""Authenticate the public mint and registration, and check mint payments on both local routes."""
import argparse
import json
import lzma
from pathlib import Path
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from replay import binary, ckb_hash, direct_dependencies, field, hex_bytes, verify
from public_index import ISSUER, SPORE, script_hash

ANCHOR = "0xd8f84d95f36c84f920cd692b7e67ff1ef67e014db518517ff4bbb396a488609b"
PROGRAM = "0xd8c6264e1c6a727a058635583803d4edc422253326ad070bb5f0fbb6a493bda8"


def read(name):
    with lzma.open(HERE / name, "rt") as stream:
        return json.load(stream)


def authenticated(stem, root, record):
    result = verify(stem, root, record, record["request"])
    result.check_returncode()
    checked = json.loads(result.stdout)
    if checked != record["verified"]:
        raise ValueError("wallet result differs from recorded history")
    return checked


def capacity(cell, lock):
    return int.from_bytes(field(cell, 0), "little") if field(cell, 1) == lock else 0


def mint_payment(record, checked):
    # All referenced transaction hashes and the mint inclusion were checked by the Rust wallet.
    parents = {ckb_hash(raw): raw for raw in map(binary, record["proof"]["references"])}
    mint = record["proof"]["steps"][0]
    tx = field(binary(mint["transaction"]), 0)
    assert hex_bytes(ckb_hash(tx)) == checked["origin"]["tx_hash"]
    matching = []
    for hash_string, index in direct_dependencies(mint):
        parent = parents.get(binary(hash_string))
        if parent is None:
            continue
        data = field(field(parent, 5), index)[4:]
        if hex_bytes(ckb_hash(data)) == PROGRAM:
            matching.append((field(field(parent, 4), index), data))
    (registration, program), = matching
    registry_type = field(registration, 2)
    args = field(registry_type, 2)[4:]
    assert len(args) == 40 and hex_bytes(field(registry_type, 0)) == ISSUER and field(registry_type, 1) == b"\x02"
    registration_hash = hex_bytes(ckb_hash(registry_type))
    assert {"type_hash": registration_hash, "data_hash": PROGRAM} in checked["required_dependencies_per_transition"]
    owner = field(registration, 1)
    inputs, outputs = field(tx, 3), field(tx, 4)
    count = int.from_bytes(inputs[:4], "little")
    assert len(inputs) == 4 + count * 44
    incoming = 0
    for offset in range(4, len(inputs), 44):
        previous = parents[inputs[offset+8:offset+40]]
        index = int.from_bytes(inputs[offset+40:offset+44], "little")
        incoming += capacity(field(field(previous, 4), index), owner)
    count = int.from_bytes(outputs[4:8], "little") // 4 - 1
    outgoing = sum(capacity(field(outputs, i), owner) for i in range(count))
    spore_index = int(checked["origin"]["index"], 16)
    spore = field(field(outputs, spore_index), 2)
    assert hex_bytes(field(spore, 0)) == SPORE
    content = field(field(tx, 5), spore_index)[4:]
    mime = field(content, 0)[4:]
    ids = {entry.strip() for param in mime.split(b";") if param.strip().startswith(b"mutant[]=")
           for entry in param.partition(b"=")[2].split(b",")}
    assert args[:32].hex().encode() in ids
    minimum = int.from_bytes(args[32:], "little")
    assert minimum == 1000 and outgoing - incoming >= minimum
    return registration_hash, program, outgoing-incoming


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    parser.add_argument("--extract-program", type=Path, help="write the authenticated testnet source to a new file")
    args = parser.parse_args()
    public = read("public.json.xz")
    inventory = read("index.json.xz")
    with tempfile.TemporaryDirectory() as scratch:
        root = Path(scratch)
        stem = args.stem.resolve()
        reg = authenticated(stem, root, public["registration"])
        member = authenticated(stem, root, public["member"])
        assert reg["anchor"] == member["anchor"] == ANCHOR
        reg_type, program, delta = mint_payment(public["member"], member)
        assert reg_type == reg["type_hash"]
        assert all(step["output"]["data_hash"] == PROGRAM for step in reg["transitions"])
        assert reg["transitions"][0]["output"]["lock_hash"] != reg["transitions"][-1]["output"]["lock_hash"]
        sample, = [row for row in inventory["members"] if row["point"] == member["origin"]]
        assert sample["data_hash"] == member["transitions"][0]["output"]["data_hash"]
        assert hex_bytes(script_hash(sample["type"])) == member["type_hash"]
        assert reg["transitions"][-1]["output"]["out_point"] in sample["registrations"]
        print("Testnet mint, net shannons to registration lock:", delta)
        for name, record in read("settlement.json.xz").items():
            checked = authenticated(stem, root, record)
            _, local_program, delta = mint_payment(record, checked)
            assert local_program == program and not checked["ended_in_burn"]
            print(name, len(checked["transitions"]), "CKB transitions, net shannons to registration lock:", delta)
    if args.extract_program:
        with args.extract_program.open("xb") as output:
            output.write(program)


if __name__ == "__main__":
    main()

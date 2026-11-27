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


def mint_payment(record, checked, *, program_hashes=(PROGRAM,), separate_payee=False):
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
        if hex_bytes(ckb_hash(data)) in program_hashes:
            matching.append((field(field(parent, 4), index), data))
    prices = {}
    type_hashes, mutant_ids, programs = set(), set(), set()
    for registration, data in matching:
        registry_type = field(registration, 2)
        args = field(registry_type, 2)[4:]
        assert len(args) == 40 and hex_bytes(field(registry_type, 0)) == ISSUER and field(registry_type, 1) == b"\x02"
        registry_hash = hex_bytes(ckb_hash(registry_type))
        assert {"type_hash": registry_hash, "data_hash": hex_bytes(ckb_hash(data))} in checked["required_dependencies_per_transition"]
        charge = int.from_bytes(args[32:], "little")
        assert charge == 1000
        owner = field(registration, 1)
        prices[owner] = prices.get(owner, 0) + charge
        type_hashes.add(registry_hash)
        mutant_ids.add(args[:32].hex().encode())
        programs.add(data)
    assert {hex_bytes(ckb_hash(data)) for data in programs} == set(program_hashes)
    assert len(matching) == len(type_hashes) == len(mutant_ids)
    inputs, outputs = field(tx, 3), field(tx, 4)
    input_count = int.from_bytes(inputs[:4], "little")
    assert len(inputs) == 4 + input_count * 44
    spent_cells = []
    for offset in range(4, len(inputs), 44):
        previous = parents[inputs[offset+8:offset+40]]
        index = int.from_bytes(inputs[offset+40:offset+44], "little")
        spent_cells.append(field(field(previous, 4), index))
    count = int.from_bytes(outputs[4:8], "little") // 4 - 1
    created = [field(outputs, i) for i in range(count)]
    fee = (sum(int.from_bytes(field(c, 0), "little") for c in spent_cells)
           - sum(int.from_bytes(field(c, 0), "little") for c in created))
    assert fee >= 0
    spore_index = int(checked["origin"]["index"], 16)
    asset_cell = field(outputs, spore_index)
    spore = field(asset_cell, 2)
    assert hex_bytes(field(spore, 0)) == SPORE
    content = field(field(tx, 5), spore_index)[4:]
    mime = field(content, 0)[4:]
    ids = {entry.strip() for param in mime.split(b";") if param.strip().startswith(b"mutant[]=")
           for entry in param.partition(b"=")[2].split(b",")}
    assert ids == mutant_ids
    credits = {}
    for owner, minimum in prices.items():
        incoming = sum(capacity(c, owner) for c in spent_cells)
        outgoing = sum(capacity(c, owner) for c in created)
        credits[owner] = outgoing - incoming
        assert credits[owner] >= minimum
        if separate_payee:
            assert field(asset_cell, 1) != owner
            spent, = [c for c in spent_cells if field(c, 1) == owner]
            successor, = [c for c in created if field(c, 1) == owner]
            assert field(spent, 2) == field(successor, 2) == b""
            assert incoming == 100 * 100_000_000 and credits[owner] == minimum
    shape = input_count, count, fee, len(tx), len(binary(mint["transaction"]))
    return type_hashes, programs, credits, shape


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
        reg_type, (program,), credits, _ = mint_payment(public["member"], member)
        delta, = credits.values()
        assert reg_type == {reg["type_hash"]}
        assert all(step["output"]["data_hash"] == PROGRAM for step in reg["transitions"])
        assert reg["transitions"][0]["output"]["lock_hash"] != reg["transitions"][-1]["output"]["lock_hash"]
        sample, = [row for row in inventory["members"] if row["point"] == member["origin"]]
        assert sample["data_hash"] == member["transitions"][0]["output"]["data_hash"]
        assert hex_bytes(script_hash(sample["type"])) == member["type_hash"]
        assert reg["transitions"][-1]["output"]["out_point"] in sample["registrations"]
        print("Testnet mint, net shannons to registration lock:", delta)
        baseline = {}
        for archive, prefix in [("settlement.json.xz", "registered-policy"), ("separate-payees.json.xz", "separate-payee"),
                                ("compact-payees.json.xz", "compact-payee")]:
            records = read(archive)
            separate = prefix != "registered-policy"
            profiles = {f"{prefix}-independent": 1, f"{prefix}-twice": 2} if separate else {prefix: 1}
            expected = {f"{profile}-{origin}": count for profile, count in profiles.items()
                        for origin in ("import", "native")}
            assert records.keys() == expected.keys()
            for name, record in records.items():
                checked = authenticated(stem, root, record)
                types, (local_program,), credits, shape = mint_payment(record, checked, separate_payee=separate)
                delta, = credits.values()
                registrations = record["settlement"]["mutant_registration"]
                entries = registrations.get("entries", [registrations])
                assert len(types) == expected[name]
                assert types == {hex_bytes(script_hash(entry["type"])) for entry in entries}
                assert local_program == program and not checked["ended_in_burn"]
                inputs, outputs, fee, body_bytes, signed_bytes = shape
                assert signed_bytes == record["settlement"]["minted"]["bytes"]
                route = name.removeprefix(prefix)
                if prefix == "separate-payee":
                    baseline[route] = shape
                if prefix == "compact-payee":
                    old_inputs, old_outputs, old_fee, old_body_bytes, old_size = baseline[route]
                    assert (inputs + 1, outputs + 1, fee) == (old_inputs, old_outputs, old_fee)
                    assert old_body_bytes - body_bytes == 153 and signed_bytes < old_size
                print(name, len(checked["transitions"]), "CKB transitions, net shannons to registration lock:", delta)
    if args.extract_program:
        with args.extract_program.open("xb") as output:
            output.write(program)


if __name__ == "__main__":
    main()

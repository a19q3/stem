"""Run the published Spore Lua extension registration on a disposable CKB devnet."""
import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
from bridge_localnet import (CKB, FAUCET_ARG, FEE, Fixtures, L1, binary, ckb_hash, dependency,
                             hex_bytes, le, output, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json

ISSUER = "94a9b875911ace20f1f0d063a26495d14e4b04e32fd218261bb747f34e71ae47"
LIB = "ed08faee8c29b7a7c29bd9d495b4b93cc207bd70ca93f7b356f39c677e7ab0fc"


def exercise(args, l1, issuer=ISSUER, fixture=None):
    if fixture is None:
        fixture = Fixtures(args)
    genesis = l1.call("get_block_by_number", "0x0")
    coinbase, groups = genesis["transactions"][:2]
    secp = dependency(point(groups["hash"], 0), "dep_group")
    index, source = next((i, o) for i, o in enumerate(coinbase["outputs"]) if o["lock"]["args"] == FAUCET_ARG)
    code, lib = args.extension.read_bytes(), args.lua.read_bytes()
    assert ckb_hash(code).hex() == issuer and ckb_hash(lib).hex() == LIB
    budgets = [(len(x) + 200) * CKB for x in (code, lib)]
    registration_budget = 600 * CKB
    amounts = [*budgets, registration_budget + FEE]
    amounts.append(int(source["capacity"], 16) - sum(amounts) - FEE)
    funding = transaction([point(coinbase["hash"], index)], [output(a, fixture.standard) for a in amounts],
                          [hex_bytes(code), hex_bytes(lib), "0x", "0x"], [secp])
    funded = l1.send(fixture.sign("deploy", funding, [wallet_group(0, [fixture.faucet])]))
    deps = [secp, dependency(point(funded, 0)), dependency(point(funded, 1))]
    type_id = ckb_hash(le(0, 8) + binary(funded) + le(2, 4) + le(0, 8))
    registry = script("0x" + issuer, hex_bytes(type_id), "data1")
    lua = hex_bytes(b"print('hello world')")
    def commit(name, proposal, key):
        signed = fixture.sign(name, proposal, [wallet_group(0, [key])])
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        return {"hash": tx, "cycles": cycles, "inclusion": l1.prove_committed(tx), "signed": signed}

    creation = transaction([point(funded, 2)], [output(registration_budget, fixture.owner_lock, registry)], [lua], deps)
    created = commit("register", creation, fixture.faucet)
    successor = transaction([point(created["hash"], 0)],
                            [output(registration_budget-FEE, fixture.friend_lock, registry)], [lua], deps)
    moved = commit("transfer-registration", successor, fixture.owner)
    returning = transaction([point(moved["hash"], 0)],
                            [output(registration_budget-2*FEE, fixture.owner_lock, registry)], [lua], deps)
    returned = commit("return-registration", returning, fixture.friend)
    head = l1.call("get_live_cell", point(returned["hash"], 0), True)
    assert head["status"] == "live" and head["cell"]["data"]["content"] == lua
    assert head["cell"]["output"]["type"] == registry and head["cell"]["output"]["lock"] == fixture.owner_lock
    request = {"type_hash": hex_bytes(script_hash(registry)), "anchor": returned["inclusion"]["header"]["hash"],
               "origin": point(created["hash"], 0), "end_transaction": returned["hash"],
               "required_states": [{"lock_hash": hex_bytes(script_hash(fixture.friend_lock)),
                                    "data_hash": hex_bytes(ckb_hash(binary(lua)))}]}
    req = args.output / "request.json"
    proof = args.output / "history.json"
    write_json(req, request)
    write_json(proof, json.loads(run(args.binary, "ckb-wallet", "prove-type-history",
                                      "--request", req, "--l1-rpc", l1.url)))
    verified = json.loads(run(args.binary, "ckb-wallet", "verify-type-history", "--request", req, "--proof", proof))
    assert len(verified["transitions"]) == 3 and len(verified["matched_states"]) == 1
    return {"genesis": genesis["header"]["hash"], "issuer_hash": "0x"+issuer, "lua_library_hash": "0x"+LIB,
            "type": registry, "created": created, "transferred": moved, "returned": returned, "live": head,
            "request": request, "history": verified}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["binary", "l1", "extension", "lua", "output"]:
        parser.add_argument("--" + name, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, required=True)
    args = parser.parse_args()
    args.output.mkdir(mode=0o700)
    node = L1(args)
    try:
        write_json(args.output / "report.json", exercise(args, node))
    finally:
        node.stop()

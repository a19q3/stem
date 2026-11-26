"""Exercise published testnet Spore 0.2.2-beta.2 with the upstream Lua example."""
import argparse
import json
from pathlib import Path

from registration import LIB, exercise
from bridge_localnet import (CKB, FEE, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes,
                             le, output, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from spore_settlement_localnet import cobuild, molecule_bytes, table

ISSUER = "5ff1a403458b436ea4b2ceb72f1fa70a6507968493315b646f5302661cb68e57"
SPORE = "685a60219309029d01310311dba953d67029170ca4848a4ff638e57002130a0d"


def assets(args, l1, registration, wallet):
    funding = json.loads((args.output / "deploy-signed.json").read_bytes())
    secp = funding["json"]["cell_deps"][0]
    code = args.spore.read_bytes()
    assert ckb_hash(code).hex() == SPORE
    available = int(funding["json"]["outputs"][3]["capacity"], 16)
    budget = 1000 * CKB
    occupied = (len(code) + 200) * CKB
    amounts = [occupied, budget, budget, available - occupied - 2 * budget - FEE]
    tx = transaction([point(funding["hash"], 3)], [output(n, wallet.standard) for n in amounts],
                     [hex_bytes(code), "0x", "0x", "0x"], [secp])
    deployed = l1.send(wallet.sign("deploy-spore", tx, [wallet_group(0, [wallet.faucet])]))
    deps = [secp, dependency(point(deployed, 0))]
    registered = [dependency(point(funding["hash"], i)) for i in (0, 1)]
    registered.append(dependency(point(registration["returned"]["hash"], 0)))

    def commit(name, tx, key):
        signed = wallet.sign(name, tx, [wallet_group(0, [key])])
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx_hash = l1.send(signed)
        return {"hash": tx_hash, "cycles": cycles, "inclusion": l1.prove_committed(tx_hash),
                "signed": signed}

    for i, label in [(1, "plain"), (2, "mutant")]:
        asset_id = ckb_hash(le(0, 8) + binary(deployed) + le(i, 4) + le(0, 8))
        ty = script("0x" + SPORE, hex_bytes(asset_id), "data1")
        mime = b"plain/test"
        asset_deps = deps
        if label == "mutant":
            mime += b";mutant[]=" + registration["type"]["args"][2:].encode()
            asset_deps = deps + registered
        data = table(molecule_bytes(mime), molecule_bytes(b"mutant spore"), b"")
        mint = transaction([point(deployed, i)], [output(budget-FEE, wallet.owner_lock, ty)],
                           [hex_bytes(data)], asset_deps)
        mint["witnesses"] = ["0x", cobuild(ty, asset_id, None, wallet.owner_lock, data=data)]
        minted = commit(label + "-mint", mint, wallet.faucet)
        move = transaction([point(minted["hash"], 0)], [output(budget-2*FEE, wallet.friend_lock, ty)],
                           [hex_bytes(data)], asset_deps)
        move["witnesses"] = ["0x", cobuild(ty, asset_id, wallet.owner_lock, wallet.friend_lock)]
        moved = commit(label + "-transfer", move, wallet.owner)
        melt = transaction([point(moved["hash"], 0)], [output(budget-3*FEE, wallet.owner_lock)],
                           ["0x"], asset_deps)
        melt["witnesses"] = ["0x", cobuild(ty, asset_id, wallet.friend_lock, None)]
        melted = commit(label + "-melt", melt, wallet.friend)
        req = {"anchor": melted["inclusion"]["header"]["hash"], "type_hash": hex_bytes(script_hash(ty)),
               "origin": point(minted["hash"], 0), "end_transaction": melted["hash"],
               "required_states": [{"lock_hash": hex_bytes(script_hash(wallet.friend_lock)),
                                    "data_hash": hex_bytes(ckb_hash(data))}]}
        req["required_dependencies"] = [{"data_hash": "0x" + SPORE}]
        if label == "mutant":
            req["required_dependencies"] += [
                {"data_hash": "0x" + ISSUER}, {"data_hash": "0x" + LIB},
                {"type_hash": hex_bytes(script_hash(registration["type"])),
                 "data_hash": hex_bytes(ckb_hash(b"print('hello world')"))},
            ]
        req_path = args.output / (label + "-request.json")
        history = args.output / (label + "-history.json")
        write_json(req_path, req)
        write_json(history, json.loads(run(args.binary, "ckb-wallet", "prove-type-history", "--request",
                                           req_path, "--l1-rpc", l1.url)))
        checked = json.loads(run(args.binary, "ckb-wallet", "verify-type-history",
                                 "--request", req_path, "--proof", history))
        assert len(checked["transitions"]) == 3 and len(checked["matched_states"]) == 1
        write_json(args.output / (label + "-report.json"), {"type": ty, "mime": mime.decode(),
                   "mint": minted, "transfer": moved, "melt": melted, "request": req, "checked": checked})


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["binary", "l1", "extension", "lua", "spore", "output"]:
        parser.add_argument("--" + name, type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, required=True)
    args = parser.parse_args()
    args.output.mkdir(mode=0o700)
    node = L1(args)
    try:
        wallet = Fixtures(args)
        registration = exercise(args, node, ISSUER, wallet)
        write_json(args.output / "registration.json", registration)
        assets(args, node, registration, wallet)
    finally:
        node.stop()

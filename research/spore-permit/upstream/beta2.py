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
CLUSTER = "0bbe768b519d8ea7b96d58f1182eb7e6ef96c541fbd9526975077ee09f049058"


def prove_history(args, l1, label, request):
    req_path = args.output / (label + "-request.json")
    history = args.output / (label + "-history.json")
    write_json(req_path, request)
    proof = run(args.binary, "ckb-wallet", "prove-type-history", "--request", req_path, "--l1-rpc", l1.url)
    write_json(history, json.loads(proof))
    return json.loads(run(args.binary, "ckb-wallet", "verify-type-history", "--request", req_path, "--proof", history))


def assets(args, l1, registration, wallet):
    funding = json.loads((args.output / "deploy-signed.json").read_bytes())
    secp = funding["json"]["cell_deps"][0]
    code = args.spore.read_bytes()
    assert ckb_hash(code).hex() == SPORE
    available = int(funding["json"]["outputs"][3]["capacity"], 16)
    budget = 1000 * CKB
    occupied = (len(code) + 200) * CKB
    amounts = [occupied, budget, budget]
    code_data = [hex_bytes(code), "0x", "0x"]
    if args.second_mutant:
        amounts += [budget, budget]
        code_data += ["0x", "0x"]
    if args.cluster:
        cluster_code = args.cluster.read_bytes()
        assert ckb_hash(cluster_code).hex() == CLUSTER
        cluster_index = len(amounts)
        amounts += [(len(cluster_code) + 200) * CKB, budget, budget]
        code_data += [hex_bytes(cluster_code), "0x", "0x"]
    amounts.append(available - sum(amounts) - FEE)
    tx = transaction([point(funding["hash"], 3)], [output(n, wallet.standard) for n in amounts],
                     code_data + ["0x"], [secp])
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

    programs = [(registration["type"], b"print('hello world')")]
    cases = [(1, "plain", 0), (2, "mutant", 1)]
    if args.second_mutant:
        index = 4
        registry_id = ckb_hash(le(0, 8) + binary(deployed) + le(index, 4) + le(0, 8))
        second = script("0x" + ISSUER, hex_bytes(registry_id), "data1")
        source = b"print('second registration')"
        creation = transaction([point(deployed, index)], [output(budget - FEE, wallet.owner_lock, second)],
                               [hex_bytes(source)], [secp, *registered[:2]])
        created = commit("second-registration", creation, wallet.faucet)
        programs.append((second, source))
        registered.append(dependency(point(created["hash"], 0)))
        cases.append((3, "two-mutants", 2))
    if args.cluster:
        cluster_id = ckb_hash(le(0, 8) + binary(deployed) + le(cluster_index + 1, 4) + le(0, 8))
        cluster_type = script("0x" + CLUSTER, hex_bytes(cluster_id), "data1")
        cluster_data = table(molecule_bytes(b"Spore Cluster"), molecule_bytes(b"Test Cluster"), b"")
        cluster_deps = [secp, dependency(point(deployed, cluster_index))]
        issue = transaction([point(deployed, cluster_index + 1)], [output(budget-FEE, wallet.standard, cluster_type)],
                            [hex_bytes(cluster_data)], cluster_deps)
        issue["witnesses"] = ["0x", cobuild(cluster_type, cluster_id, None, wallet.standard, data=cluster_data, cluster=True)]
        collection = commit("cluster-mint", issue, wallet.faucet)
        cases.append((cluster_index + 2, "cluster-member", 0))
    for i, label, count in cases:
        asset_id = ckb_hash(le(0, 8) + binary(deployed) + le(i, 4) + le(0, 8))
        ty = script("0x" + SPORE, hex_bytes(asset_id), "data1")
        mime = b"plain/test"
        asset_deps = deps
        if count:
            mime += b";mutant[]=" + b",".join(r["args"][2:].encode() for r, _ in programs[:count])
            asset_deps = deps + registered[:2 + count]
        member = label == "cluster-member"
        if member:
            asset_deps = deps + cluster_deps[1:] + [dependency(point(collection["hash"], 0))]
        data = table(molecule_bytes(mime), molecule_bytes(b"mutant spore"), molecule_bytes(cluster_id) if member else b"")
        capacity = budget - (120 * CKB if member else 0)
        mint_outputs = [output(capacity-FEE, wallet.owner_lock, ty)]
        if member:
            mint_outputs.append(output(120 * CKB, wallet.standard))
        mint = transaction([point(deployed, i)], mint_outputs, [hex_bytes(data)] + ["0x"] * int(member), asset_deps)
        mint["witnesses"] = ["0x", cobuild(ty, asset_id, None, wallet.owner_lock, data=data)]
        minted = commit(label + "-mint", mint, wallet.faucet)
        move = transaction([point(minted["hash"], 0)], [output(capacity-2*FEE, wallet.friend_lock, ty)],
                           [hex_bytes(data)], asset_deps)
        move["witnesses"] = ["0x", cobuild(ty, asset_id, wallet.owner_lock, wallet.friend_lock)]
        moved = commit(label + "-transfer", move, wallet.owner)
        melt = transaction([point(moved["hash"], 0)], [output(capacity-3*FEE, wallet.owner_lock)],
                           ["0x"], asset_deps)
        melt["witnesses"] = ["0x", cobuild(ty, asset_id, wallet.friend_lock, None)]
        melted = commit(label + "-melt", melt, wallet.friend)
        req = {"anchor": melted["inclusion"]["header"]["hash"], "type_hash": hex_bytes(script_hash(ty)),
               "origin": point(minted["hash"], 0), "end_transaction": melted["hash"],
               "required_states": [{"lock_hash": hex_bytes(script_hash(wallet.friend_lock)),
                                    "data_hash": hex_bytes(ckb_hash(data))}]}
        req["required_dependencies"] = [{"data_hash": "0x" + SPORE}]
        if count:
            req["required_dependencies"] += [
                {"data_hash": "0x" + ISSUER}, {"data_hash": "0x" + LIB},
                *[{"type_hash": hex_bytes(script_hash(r)), "data_hash": hex_bytes(ckb_hash(source))}
                  for r, source in programs[:count]],
            ]
        if member:
            req["required_dependencies"] += [{"data_hash": "0x" + CLUSTER}, {
                "type_hash": hex_bytes(script_hash(cluster_type)), "data_hash": hex_bytes(ckb_hash(cluster_data))}]
        checked = prove_history(args, l1, label, req)
        assert len(checked["transitions"]) == 3 and len(checked["matched_states"]) == 1
        write_json(args.output / (label + "-report.json"), {"type": ty, "mime": mime.decode(),
                   "mint": minted, "transfer": moved, "melt": melted, "request": req, "checked": checked})
    if args.cluster:
        update = transaction([point(collection["hash"], 0)], [output(budget-2*FEE, wallet.friend_lock, cluster_type)],
                             [hex_bytes(cluster_data)], cluster_deps)
        update["witnesses"] = ["0x", cobuild(cluster_type, cluster_id, wallet.standard, wallet.friend_lock, cluster=True)]
        transferred = commit("cluster-transfer", update, wallet.faucet)
        back = transaction([point(transferred["hash"], 0)], [output(budget-3*FEE, wallet.standard, cluster_type)],
                           [hex_bytes(cluster_data)], cluster_deps)
        back["witnesses"] = ["0x", cobuild(cluster_type, cluster_id, wallet.friend_lock, wallet.standard, cluster=True)]
        returned = commit("cluster-return", back, wallet.friend)
        req = {"anchor": returned["inclusion"]["header"]["hash"], "type_hash": hex_bytes(script_hash(cluster_type)),
               "origin": point(collection["hash"], 0), "end_transaction": returned["hash"],
               "required_states": [{"lock_hash": hex_bytes(script_hash(wallet.friend_lock)), "data_hash": hex_bytes(ckb_hash(cluster_data))}],
               "required_dependencies": [{"data_hash": "0x" + CLUSTER}]}
        checked = prove_history(args, l1, "collection", req)
        assert len(checked["transitions"]) == 3 and len(checked["matched_states"]) == 1 and not checked["ended_in_burn"]
        write_json(args.output / "collection-report.json", {"type": cluster_type, "data": hex_bytes(cluster_data), "mint": collection,
                   "transfer": transferred, "return": returned, "request": req, "checked": checked})


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["binary", "l1", "extension", "lua", "spore", "output"]:
        parser.add_argument("--" + name, type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, required=True)
    parser.add_argument("--second-mutant", action="store_true", help="also exercise two registered programs on the same Spore")
    parser.add_argument("--cluster", type=Path, help="published beta.2 Cluster binary for the plain collection and membership controls")
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

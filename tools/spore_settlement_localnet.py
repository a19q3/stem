"""Verify real Spore V2 custody, Stem ownership and CKB redemption."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, l1_reference_request, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell
from stake_localnet import witness

# Published Spore V2 mainnet code and the CoBuild ScriptInfo hash used by CCC.
SPORE_CODE = bytes.fromhex("4a4dce1df3dffff7f8b2cd7dff7303df3b6150c9788cb75dcf6747247132b9f5")
CLUSTER_CODE = bytes.fromhex("7366a61534fa7c7e6225ecc0d828ea3b5366adec2b58206f2ee84995fe030075")
SCRIPT_INFO = bytes.fromhex("23cde495cfd125019140ea933e76e3cbf16189483232ab5a05d88fb34d1023e1")


def table(*fields):
    size = 4 + 4 * len(fields)
    offsets = []
    for field in fields:
        offsets.append(le(size, 4))
        size += len(field)
    return le(size, 4) + b"".join(offsets) + b"".join(fields)


def molecule_bytes(data):
    return le(len(data), 4) + data


def cobuild(object_type, object_id, source, target, *, data=None, cluster=False):
    to = le(0, 4) + packed_script(target)
    tag = 3 if cluster else 0
    if source is None:
        detail = le(tag, 4) + table(object_id, to, ckb_hash(data))
    else:
        detail = le(tag + 1, 4) + table(object_id, le(0, 4) + packed_script(source), to)
    action = table(SCRIPT_INFO, script_hash(object_type), molecule_bytes(detail))
    return hex_bytes(le(0xff000001, 4) + table(molecule_bytes(b""), table(table(action))))


def spore_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    genesis = vault["genesis"]
    network = client.network
    empty = bytes(32)
    code = args.spore_v2.read_bytes()
    assert ckb_hash(code) == SPORE_CODE, "the fixture requires the published Spore V2 ELF"
    mailbox_bin = args.intercell_mailbox.read_bytes()
    ckb_lock_bin = args.ckb_spore_escrow.read_bytes()
    stem_spore_code = hex_bytes(ckb_hash(args.stem_spore.read_bytes()))
    native_lock_code = hex_bytes(ckb_hash(args.native_lock.read_bytes()))
    owner = script(native_lock_code, fixture.key(fixture.owner)["blake160"], "data")
    buyer = script(native_lock_code, fixture.key(fixture.friend)["blake160"], "data")
    mailbox_code, escrow_code = [ckb_hash(program) for program in (mailbox_bin, ckb_lock_bin)]
    secp = vault["secp"]
    spore_cap, mailbox_cap, relay_funds = 360 * CKB, 1000 * CKB, 1800 * CKB
    codes = [code, mailbox_bin, ckb_lock_bin]
    cluster_code = args.spore_cluster_v2.read_bytes() if args.spore_cluster_v2 else None
    cluster_cap, proxy_cap = 300 * CKB, 120 * CKB if cluster_code else 0
    if cluster_code:
        assert ckb_hash(cluster_code) == CLUSTER_CODE, "the fixture requires the published Cluster ELF"
        codes.append(cluster_code)
    capacities = [(len(binary_) + 100) * CKB for binary_ in codes]
    funds = [(spore_cap + proxy_cap + FEE, fixture.standard), (mailbox_cap + FEE, fixture.standard),
             (relay_funds, fixture.standard)]
    if cluster_code:
        funds += [(cluster_cap + FEE, fixture.standard), (spore_cap + proxy_cap + FEE, fixture.owner_lock)]
    fund = transaction([vault["change_point"]],
        [output(amount, fixture.standard) for amount in capacities] +
        [output(amount, lock) for amount, lock in funds] +
        [output(vault["change"] - sum(capacities) - sum(amount for amount, _ in funds) - FEE,
                fixture.standard)],
        [hex_bytes(binary_) for binary_ in codes] + ["0x"] * (len(funds) + 1), [secp])
    funded = l1.send(fixture.sign("spore-fund", fund, [wallet_group(0, [fixture.faucet])]))
    deps = [secp] + [dependency(point(funded, i)) for i in range(3)]

    def estimate(signed):
        return int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)

    def submit_ckb(signed, cycles=None):
        if cycles is None:
            cycles = estimate(signed)
        tx = l1.send(signed)
        return {"hash": tx, "inclusion": l1.prove_committed(tx), "cycles": cycles,
                "bytes": len(binary(signed["molecule"]))}

    def ckb_reject(tx, code=None):
        error = l1.reject(tx)
        if code is not None:
            assert f"error code {code}" in error, error
        return error

    def type_id(index):
        return ckb_hash(le(0, 8) + binary(funded) + le(index, 4) + le(0, 8))

    mint_source = len(codes)
    cluster_id, cluster = b"", None
    rejected = {}
    if cluster_code:
        cluster_id = type_id(mint_source + 3)
        cluster_type = script(hex_bytes(CLUSTER_CODE), hex_bytes(cluster_id), "data1")
        cluster_data = table(molecule_bytes(b"Cross-chain collection"), molecule_bytes(b"CKB custody"), b"")
        cluster_dep = dependency(point(funded, 3))
        creation = transaction([point(funded, mint_source + 3)],
            [output(cluster_cap, fixture.standard, cluster_type)], [hex_bytes(cluster_data)],
            [secp, cluster_dep])
        creation["witnesses"] = ["0x", cobuild(cluster_type, cluster_id, None, fixture.standard,
                                                  data=cluster_data, cluster=True)]
        created_cluster = submit_ckb(fixture.sign("spore-cluster-mint", creation, [wallet_group(0, [fixture.faucet])]))
        cluster_point = point(created_cluster["hash"], 0)
        cluster = {"id": hex_bytes(cluster_id), "type": cluster_type, "minted": created_cluster}

    object_id = type_id(mint_source)
    object_type = script(hex_bytes(SPORE_CODE), hex_bytes(object_id), "data1")
    original = script_hash(object_type)
    def spore_data(id_):
        return table(molecule_bytes(b"text/plain"), molecule_bytes(b"One Spore across two networks"),
                     molecule_bytes(id_) if id_ else b"")

    data = spore_data(cluster_id)

    def mint_spore(label, index=mint_source, key=fixture.faucet, include_cluster=True):
        id_ = type_id(index)
        type_ = {**object_type, "args": hex_bytes(id_)}
        outputs = [output(spore_cap, fixture.owner_lock, type_)]
        payload, dependencies = [hex_bytes(data)], deps.copy()
        if cluster:
            outputs.append(output(proxy_cap, fixture.standard))
            payload.append("0x")
            if include_cluster:
                dependencies.append(dependency(cluster_point))
        tx = transaction([point(funded, index)], outputs, payload, dependencies)
        tx["witnesses"] = ["0x", cobuild(type_, id_, None, fixture.owner_lock, data=data)]
        return fixture.sign(f"spore-{label}", tx, [wallet_group(0, [key])])

    if cluster:
        for label, tx in (
            ("mint_without_cluster", mint_spore("no-cluster", include_cluster=False)),
            ("mint_without_cluster_authority", mint_spore("no-cluster-authority", mint_source + 4, fixture.owner)),
        ):
            rejected[f"ckb_{label}"] = ckb_reject(tx)
            assert SPORE_CODE.hex() in rejected[f"ckb_{label}"], rejected[f"ckb_{label}"]
    minted = submit_ckb(mint_spore("mint"))
    object_point = point(minted["hash"], 0)
    sequencer_id = type_id(mint_source + 1)
    checkpoint_code, checkpoint_id = binary(vault["anchor"]["hash"]), vault["anchor"]["id"]
    endpoint = script(stem_spore_code, hex_bytes(original + binary(genesis) + mailbox_code
        + sequencer_id + escrow_code + checkpoint_code + checkpoint_id), "data")
    endpoint_id = script_hash(endpoint)
    ckb_escrow = script(hex_bytes(escrow_code),
        hex_bytes(sequencer_id + endpoint_id + original + mailbox_code), "data2")
    mailbox = script(hex_bytes(mailbox_code), hex_bytes(sequencer_id + binary(genesis) + binary(network)
        + script_hash(ckb_escrow) + checkpoint_code + checkpoint_id), "data2")
    mailbox_id = script_hash(mailbox)
    mapped_type = script(stem_spore_code, hex_bytes(endpoint_id), "data")
    initial = transaction([point(funded, mint_source + 1)], [output(mailbox_cap, ckb_escrow, mailbox)],
                          [hex_bytes(le(0, 8) + empty)], deps)
    initial["header_deps"] = [genesis]
    created = l1.send(fixture.sign("spore-mailbox", initial, [wallet_group(0, [fixture.faucet])]))
    mailbox_point = point(created, 0)
    l1_fee_point = point(funded, mint_source + 2)
    fee_capacity = relay_funds
    nonce_frontier = 0

    def packet(label, outbound, recipient, nonce, content=data):
        source, destination, sender, receiver = ((network, genesis, endpoint_id, mailbox_id) if outbound
                                                 else (genesis, network, mailbox_id, endpoint_id))
        path = args.output / f"spore-{label}-message.json"
        write_json(path, dict(source=source, destination=destination, sender=hex_bytes(sender),
            receiver=hex_bytes(receiver), nonce=nonce, payload=hex_bytes(original + recipient + content)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", path))

    importing = packet("deposit", False, script_hash(owner), 0)

    def deposit(label, *, move=True, content=data, policy=importing,
                from_lock=fixture.owner_lock, key=fixture.owner, consumed=empty, include_cobuild=True):
        inputs = [mailbox_point, object_point, l1_fee_point] if move else [mailbox_point, l1_fee_point]
        outputs = [output(mailbox_cap, ckb_escrow, mailbox)]
        payload = [hex_bytes(le(nonce_frontier + 1, 8) + consumed + binary(policy["record"]))]
        if move:
            outputs.append(output(spore_cap, ckb_escrow, object_type))
            payload.append(hex_bytes(content))
        outputs.append(output(fee_capacity - FEE, fixture.standard))
        tx = transaction(inputs, outputs, payload + ["0x"], deps)
        if move:
            if include_cobuild:
                tx["witnesses"] = ["0x"] * len(inputs) + [
                    cobuild(object_type, object_id, from_lock, ckb_escrow)]
            groups = [wallet_group(1, [key]), wallet_group(2, [fixture.faucet])]
        else:
            groups = [wallet_group(1, [fixture.faucet])]
        return fixture.sign(f"spore-{label}", tx, groups)

    held_signed = deposit("deposit")
    held_cycles = estimate(held_signed)
    rejected["ckb_deposit_without_original"] = ckb_reject(deposit("fake-deposit", move=False), 111)
    rejected["ckb_missing_spore_cobuild"] = ckb_reject(deposit("no-cobuild", include_cobuild=False))
    assert SPORE_CODE.hex() in rejected["ckb_missing_spore_cobuild"], rejected["ckb_missing_spore_cobuild"]
    rejected["ckb_mutated_original"] = ckb_reject(deposit("mutated-deposit", content=data[:-1] + b"?"))
    wrong_cluster = spore_data(ckb_hash(b"A different Cluster"))
    rejected["ckb_forged_cluster"] = ckb_reject(deposit("forged-cluster",
        policy=packet("forged-cluster", False, script_hash(owner), 0, wrong_cluster)), 113)
    if cluster:
        rejected["ckb_removed_cluster"] = ckb_reject(deposit("removed-cluster",
            policy=packet("removed-cluster", False, script_hash(owner), 0, spore_data(b""))), 113)
    rejected["ckb_deposit_to_stem_endpoint"] = ckb_reject(deposit("deposit-to-stem-endpoint",
        policy=packet("deposit-to-stem-endpoint", False, endpoint_id, 0)), 113)
    held = submit_ckb(held_signed, held_cycles)
    mailbox_point, object_point, l1_fee_point = [point(held["hash"], i) for i in range(3)]
    fee_capacity -= FEE
    nonce_frontier += 1
    assert l1.call("get_live_cell", point(minted["hash"], 0), True)["status"] != "live"

    def query(path, data="0x"):
        proof = client.query(path, data=data, prove=True)
        return proof["height"], json.loads(bytes.fromhex(proof["value"]))

    def certified(path, data):
        height, proof = query(path, data)
        return {**proof, "height": height, "finality": client.finality(height)["proof"]}

    def cell_proof(location):
        key = hex_bytes(binary(location["tx_hash"]) + le(int(location["index"], 16), 4))
        return {**certified("/stem/cell", key), "point": key}

    def commit(tx):
        result = client.call("broadcast_tx_commit", tx=tx)
        assert result["check_tx"]["code"] == 0, result
        return result

    def stem_reject(tx, reason):
        result = client.call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and reason in result["log"], result
        return result["log"]

    def attest(command, name, params, *, journal=None):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            votes.append(json.loads(run(args.binary, "bridge", command, "--genesis", spec_path,
                "--l1-rpc", l1.url, "--key-file", key, "--journal",
                args.output / f"spore-{journal or name}-{index}", *params)))
        path = args.output / f"spore-{name}-approvals.json"
        write_json(path, votes)
        return path

    def observe_deposit(label, deposit_):
        l1.mine(spec["bridge"]["confirmations"])
        _, prior_anchor = query("/stem/bridge-anchor")
        ckb_header = deposit_["inclusion"]["header"]
        height = int(ckb_header["number"], 16)
        approvals = attest("attest-anchor", f"{label}-anchor", ["--previous-height", str(prior_anchor["height"]),
            "--previous-hash", prior_anchor["hash"], "--height", str(height)], journal="anchor")
        anchor = commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", approvals)))
        reference = args.output / f"spore-{label}-reference-request.json"
        write_json(reference, l1_reference_request(height, ckb_header["hash"], (point(deposit_["hash"], 0), False)))
        approved = attest("attest-reference", f"{label}-reference", ["--request", reference], journal="reference")
        return anchor, approved

    anchor, approved = observe_deposit("import", held)
    code_points = [mint["lock_dep"], point(mint["mint_id"], 2)]

    def stem_signed(label, spending, outputs, *, actor=owner, key=fixture.owner, bond=None,
                    message=None, receive=False, observations=None):
        points = [spending] if bond is None else [spending, bond]
        locks = [actor] if bond is None else sorted((actor, endpoint), key=packed_script)
        request = {"inputs": [{"out_point": p} for p in points], "outputs": outputs,
                   "cell_deps": [dependency(p) for p in code_points]}
        if message:
            request["messages"] = [{"group_index": locks.index(endpoint), "actions": [
                {"kind": 0xff01 if receive else 0xff00, "data": message["record"]}]}]
        req, draft, context, lock = [args.output / f"spore-{label}-{suffix}.json"
                                   for suffix in ("wallet", "draft", "context", "lock")]
        write_json(req, request)
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", req)))
        if observations:
            attached = args.output / f"spore-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references", "--genesis", spec_path,
                "--transaction", draft, "--observations", observations)))
            draft = attached
        write_json(context, {"cells": [signing_cell(client, p) for p in points + code_points]})
        write_json(lock, actor)
        tx = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path, "--transaction", draft,
                          "--context", context, "--group-index", str(locks.index(actor)), "--lock", lock,
                          "--key-file", key))
        write_json(args.output / f"spore-{label}-stem-signed.json", tx)
        return tx

    def at(tx, index):
        return point("0x" + tx["hash"], index)

    bond, mapped_cap = 100 * CKB, 80 * CKB
    source_cap = mint["payment_capacity"] - bond - FEE
    initialized = commit(stem_signed("create-endpoint", mint["payment"],
        [cell(bond, endpoint), cell(source_cap, owner)]))
    bond_at, funding = at(initialized, 0), at(initialized, 1)
    mapped = cell(mapped_cap, owner, data, [mapped_type])
    change = cell(source_cap - mapped_cap - FEE, owner)
    rejected["stem_forged_issuance"] = stem_reject(stem_signed("no-custody", funding, [mapped, change]), "exited with 107")
    rejected["stem_changed_recipient"] = stem_reject(stem_signed("redirect-import", funding,
        [cell(bond, endpoint), cell(mapped_cap, buyer, data, [mapped_type]), change],
        bond=bond_at, message=importing, receive=True, observations=approved), "exited with 109")
    rejected["stem_changed_import_content"] = stem_reject(stem_signed("mutate-import", funding,
        [cell(bond, endpoint), cell(mapped_cap, owner, data[:-1] + b"?", [mapped_type]), change],
        bond=bond_at, message=importing, receive=True, observations=approved), "exited with 109")
    rejected["stem_duplicate_output"] = stem_reject(stem_signed("duplicate-import", funding,
        [cell(bond, endpoint), mapped, mapped, cell(source_cap - mapped_cap * 2 - FEE, owner)],
        bond=bond_at, message=importing, receive=True, observations=approved), "exited with 109")
    imported = commit(stem_signed("import", funding, [cell(bond, endpoint), mapped, change],
        bond=bond_at, message=importing, receive=True, observations=approved))
    bond_at, mapped_at, funding = at(imported, 0), at(imported, 1), at(imported, 2)
    rejected["stem_import_replay"] = stem_reject(stem_signed("import-replay", funding,
        [cell(bond, endpoint), mapped, cell(source_cap - mapped_cap * 2 - 2 * FEE, owner)],
        bond=bond_at, message=importing, receive=True, observations=approved), "nonce already used")

    rejected["stem_mutated_transfer"] = stem_reject(stem_signed("modify", mapped_at,
        [cell(mapped_cap - FEE, buyer, data[:-1] + b"?", [mapped_type])]), "exited with 107")
    if cluster:
        rejected["stem_removed_cluster"] = stem_reject(stem_signed("remove-cluster", mapped_at,
            [cell(mapped_cap - FEE, buyer, spore_data(b""), [mapped_type])]), "exited with 107")
        tx = transaction([cluster_point, l1_fee_point],
            [output(cluster_cap, fixture.friend_lock, cluster_type), output(fee_capacity - FEE, fixture.standard)],
            [hex_bytes(cluster_data), "0x"], [secp, cluster_dep])
        tx["witnesses"] = ["0x", "0x", cobuild(cluster_type, cluster_id, fixture.standard,
                                                   fixture.friend_lock, cluster=True)]
        group = {**wallet_group(0, [fixture.faucet]), "inputs": [0, 1]}
        cluster["transferred"] = submit_ckb(fixture.sign("spore-cluster-transfer", tx, [group]))
        l1_fee_point = point(cluster["transferred"]["hash"], 1)
        fee_capacity -= FEE
        assert l1.call("get_live_cell", cluster_point, True)["status"] != "live"
    transferred = commit(stem_signed("transfer", mapped_at, [cell(mapped_cap - FEE, buyer, data, [mapped_type])]))
    mapped_at = at(transferred, 0)
    nonce = int.from_bytes(ckb_hash(b"Stem Spore return nonce\0" + binary(bond_at["tx_hash"])
                                  + le(int(bond_at["index"], 16), 4))[:8], "little")
    returning = packet("return", True, script_hash(fixture.friend_lock), nonce)
    returned_outputs = [cell(bond, endpoint), cell(mapped_cap - 2 * FEE, buyer)]
    rejected["stem_burn_without_return"] = stem_reject(stem_signed("no-return", mapped_at,
        [cell(mapped_cap - 2 * FEE, buyer)], actor=buyer, key=fixture.friend), "exited with 107")
    rejected["stem_return_without_object"] = stem_reject(stem_signed("return-without-object", funding,
        [cell(bond, endpoint), cell(source_cap - mapped_cap - 2 * FEE, owner)],
        bond=bond_at, message=returning), "exited with 109")
    rejected["stem_return_to_ckb_escrow"] = stem_reject(stem_signed("return-to-ckb-escrow", mapped_at,
        returned_outputs, actor=buyer, key=fixture.friend, bond=bond_at,
        message=packet("return-to-ckb-escrow", True, script_hash(ckb_escrow), nonce)), "exited with 108")
    burned = commit(stem_signed("return", mapped_at, returned_outputs,
        actor=buyer, key=fixture.friend, bond=bond_at, message=returning))

    proof_height, outbox = query("/stem/intercell/outbox", returning["id"])
    checkpoint = relay.advance(proof_height, secp, probe=True)
    header, request = [args.output / f"spore-outbox-{name}.json" for name in ("header", "request")]
    write_json(header, hex_bytes(relay.header(proof_height)))
    write_json(request, {**outbox, "consumed": [], "expected_root": hex_bytes(empty)})
    encoded = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
    write_json(args.output / "spore-ckb-delivery-witness.json", encoded)
    path = binary(encoded["input_type"])

    def redeem(label, recipient=fixture.friend_lock, *, include_checkpoint=True, capacity=spore_cap):
        tx = transaction([mailbox_point, object_point, l1_fee_point],
            [output(mailbox_cap, ckb_escrow, mailbox), output(capacity, recipient, object_type),
             output(fee_capacity - FEE + spore_cap - capacity, fixture.standard)],
            [hex_bytes(le(nonce_frontier, 8) + binary(encoded["successor_root"])), hex_bytes(data), "0x"],
            deps + ([checkpoint] if include_checkpoint else []))
        tx["witnesses"] = [witness(path), "0x", "0x",
                           cobuild(object_type, object_id, ckb_escrow, recipient)]
        return fixture.sign(f"spore-{label}", tx, [wallet_group(2, [fixture.faucet])])

    rejected["ckb_release_without_checkpoint"] = ckb_reject(redeem("no-checkpoint", include_checkpoint=False), 84)
    rejected["ckb_release_wrong_owner"] = ckb_reject(redeem("wrong-owner", recipient=fixture.owner_lock), 113)
    rejected["ckb_release_capacity_drain"] = ckb_reject(redeem("capacity-drain", capacity=spore_cap - CKB), 112)
    released = submit_ckb(redeem("release"))
    final = l1.call("get_live_cell", point(released["hash"], 1), True)["cell"]
    assert final["output"] == output(spore_cap, fixture.friend_lock, object_type)
    assert final["data"]["content"] == hex_bytes(data)
    assert l1.call("get_live_cell", object_point, True)["status"] != "live"
    end_cell = cell_proof(mapped_at)
    assert end_cell["cell"] is None

    mailbox_point, object_point, l1_fee_point = [point(released["hash"], i) for i in range(3)]
    fee_capacity -= FEE
    redeposit_message = packet("redeposit", False, script_hash(buyer), nonce_frontier)
    redeposited = submit_ckb(deposit("redeposit", policy=redeposit_message, from_lock=fixture.friend_lock,
                                   key=fixture.friend, consumed=binary(encoded["successor_root"])))
    mailbox_point, object_point, l1_fee_point = [point(redeposited["hash"], i) for i in range(3)]
    fee_capacity -= FEE
    nonce_frontier += 1
    rejected["ckb_replay_on_new_custody"] = ckb_reject(redeem("replay-on-new-custody"), 85)
    second_anchor, second_approval = observe_deposit("reimport", redeposited)
    revived = cell(mapped_cap, buyer, data, [mapped_type])
    reimported = commit(stem_signed("reimport", funding, [cell(bond, endpoint), revived,
        cell(source_cap - mapped_cap * 2 - 2 * FEE, owner)], bond=at(burned, 0), message=redeposit_message,
        receive=True, observations=second_approval))
    revived_cell = signing_cell(client, at(reimported, 1))
    assert revived_cell["cell"] == revived
    assert l1.call("get_live_cell", object_point, True)["cell"]["output"] == output(spore_cap, ckb_escrow, object_type)
    if cluster:
        cluster["live_after_reimport"] = l1.call("get_live_cell", point(cluster["transferred"]["hash"], 0), True)["cell"]
        assert cluster["live_after_reimport"]["output"] == output(cluster_cap, fixture.friend_lock, cluster_type)
        assert cluster["live_after_reimport"]["data"]["content"] == hex_bytes(cluster_data)

    def execution(committed):
        location = client.locations[committed["hash"]]
        height = location["height"]
        response = client.query("/stem/execution", height=height,
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return {**json.loads(bytes.fromhex(response["value"])), "height": height,
                "finality": client.finality(height)["proof"]}

    return {"id": hex_bytes(object_id), "original_type": object_type, "mapped_type": mapped_type, "cluster": cluster,
            "mailbox_creation": l1.prove_committed(created), "mailbox": mailbox, "endpoint": endpoint,
            "minted": minted, "held": held, "anchor": anchor, "imported": imported,
            "transferred": transferred, "burned": burned, "released": released, "final_ckb": final,
            "locked_bond": signing_cell(client, at(reimported, 0)), "spent_mapping": end_cell,
            "outbox": certified("/stem/intercell/outbox", returning["id"]),
            "inbox": certified("/stem/intercell/inbox", importing["id"]),
            "redeposited": redeposited, "second_anchor": second_anchor,
            "reimported": reimported, "reimported_cell": revived_cell,
            "reimported_proof": cell_proof(at(reimported, 1)),

            "execution": {name: execution(tx) for name, tx in (
                ("imported", imported), ("transferred", transferred), ("burned", burned), ("reimported", reimported))},
            "rejections": rejected}

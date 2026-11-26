"""Verify real Spore V2 custody, Stem ownership and CKB redemption."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, l1_reference_request, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell
from stake_localnet import witness

# Published Spore V2 mainnet code and the CoBuild ScriptInfo hash used by CCC.
SPORE_CODE = bytes.fromhex("4a4dce1df3dffff7f8b2cd7dff7303df3b6150c9788cb75dcf6747247132b9f5")
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


def cobuild(spore, object_id, source, target, *, data=None):
    to = le(0, 4) + packed_script(target)
    if source is None:
        detail = le(0, 4) + table(object_id, to, ckb_hash(data))
    else:
        detail = le(1, 4) + table(object_id, le(0, 4) + packed_script(source), to)
    action = table(SCRIPT_INFO, script_hash(spore), molecule_bytes(detail))
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
    capacities = [(len(binary_) + 100) * CKB for binary_ in codes]
    fund = transaction([vault["change_point"]],
        [output(amount, fixture.standard) for amount in capacities] +
        [output(spore_cap + FEE, fixture.standard), output(mailbox_cap + FEE, fixture.standard),
         output(relay_funds, fixture.standard),
         output(vault["change"] - sum(capacities) - spore_cap - mailbox_cap - relay_funds - 3 * FEE,
                fixture.standard)],
        [hex_bytes(binary_) for binary_ in codes] + ["0x"] * 4, [secp])
    funded = l1.send(fixture.sign("spore-fund", fund, [wallet_group(0, [fixture.faucet])]))
    deps = [secp] + [dependency(point(funded, i)) for i in range(3)]

    def submit_ckb(signed):
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
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

    object_id = type_id(3)
    object_type = script(hex_bytes(SPORE_CODE), hex_bytes(object_id), "data1")
    original = script_hash(object_type)
    data = table(molecule_bytes(b"text/plain"), molecule_bytes(b"One Spore across two networks"), b"")
    creator = transaction([point(funded, 3)], [output(spore_cap, fixture.owner_lock, object_type)],
                          [hex_bytes(data)], deps)
    creator["witnesses"] = ["0x", cobuild(object_type, object_id, None, fixture.owner_lock, data=data)]
    minted = submit_ckb(fixture.sign("spore-mint", creator, [wallet_group(0, [fixture.faucet])]))
    object_point = point(minted["hash"], 0)
    sequencer_id = type_id(4)
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
    initial = transaction([point(funded, 4)], [output(mailbox_cap, ckb_escrow, mailbox)],
                          [hex_bytes(le(0, 8) + empty)], deps)
    initial["header_deps"] = [genesis]
    created = l1.send(fixture.sign("spore-mailbox", initial, [wallet_group(0, [fixture.faucet])]))
    mailbox_point = point(created, 0)
    l1_fee_point = point(funded, 5)
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

    rejected = {}
    rejected["ckb_deposit_without_original"] = ckb_reject(deposit("fake-deposit", move=False), 111)
    rejected["ckb_missing_spore_cobuild"] = ckb_reject(deposit("no-cobuild", include_cobuild=False))
    assert SPORE_CODE.hex() in rejected["ckb_missing_spore_cobuild"], rejected["ckb_missing_spore_cobuild"]
    rejected["ckb_mutated_original"] = ckb_reject(deposit("mutated-deposit", content=data[:-1] + b"?"))
    clustered = table(molecule_bytes(b"text/plain"), molecule_bytes(b"unsupported"), molecule_bytes(bytes(32)))
    rejected["ckb_cluster_payload"] = ckb_reject(deposit("unsupported-cluster",
        policy=packet("unsupported-cluster", False, script_hash(owner), 0, clustered)), 113)
    rejected["ckb_deposit_to_stem_endpoint"] = ckb_reject(deposit("deposit-to-stem-endpoint",
        policy=packet("deposit-to-stem-endpoint", False, endpoint_id, 0)), 113)
    held = submit_ckb(deposit("deposit"))
    mailbox_point, object_point, l1_fee_point = [point(held["hash"], i) for i in range(3)]
    fee_capacity -= FEE
    nonce_frontier += 1
    assert l1.call("get_live_cell", point(minted["hash"], 0), True)["status"] != "live"

    def query(path, data="0x"):
        proof = client.query(path, data=data, prove=True)
        return proof["height"], json.loads(bytes.fromhex(proof["value"]))

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
    end_cell = client.query("/stem/cell", data=hex_bytes(binary(mapped_at["tx_hash"]) + le(0, 4)), prove=True)
    assert json.loads(bytes.fromhex(end_cell["value"]))["cell"] is None

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

    def execution(committed):
        location = client.locations[committed["hash"]]
        response = client.query("/stem/execution", height=location["height"],
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return json.loads(bytes.fromhex(response["value"]))

    return {"id": hex_bytes(object_id), "original_type": object_type, "mapped_type": mapped_type,
            "mailbox_creation": l1.prove_committed(created), "mailbox": mailbox, "endpoint": endpoint,
            "minted": minted, "held": held, "anchor": anchor, "imported": imported,
            "transferred": transferred, "burned": burned, "released": released, "final_ckb": final,
            "locked_bond": signing_cell(client, at(reimported, 0)), "spent_mapping": end_cell,
            "outbox": outbox, "inbox": query("/stem/intercell/inbox", importing["id"])[1],
            "redeposited": redeposited, "second_anchor": second_anchor,
            "reimported": reimported, "reimported_cell": revived_cell,

            "execution": {name: execution(tx) for name, tx in (
                ("imported", imported), ("transferred", transferred), ("burned", burned), ("reimported", reimported))},
            "rejections": rejected}

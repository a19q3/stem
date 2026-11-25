"""Export a native two-Type object, enforce its policy on CKB and return it."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell
from spore_settlement_localnet import molecule_bytes, table
from stake_localnet import witness


def object_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    network, genesis = client.network, vault["genesis"]
    empty = bytes(32)
    owner_key, buyer_key, authority_key = fixture.owner, fixture.friend, fixture.l1_keys[0]
    ckb_authority = dict(fixture.standard, args=fixture.key(authority_key)["blake160"])
    authority_id = script_hash(ckb_authority)
    native_code = hex_bytes(ckb_hash(args.native_lock.read_bytes()))
    native_scripts = [script(native_code, fixture.key(key)["blake160"], "data")
                      for key in (owner_key, buyer_key, authority_key)]
    owner, buyer, authority = native_scripts
    mailbox_bin, ckb_program = [path.read_bytes() for path in (args.intercell_mailbox, args.ckb_stem_object)]
    mailbox_code, ckb_code = [ckb_hash(program) for program in (mailbox_bin, ckb_program)]
    native_object = hex_bytes(ckb_hash(args.stem_native_object.read_bytes()))
    policy_code = hex_bytes(ckb_hash(args.object_policy.read_bytes()))
    secp = vault["secp"]
    mailbox_cap, authority_funds, token_cap = 1000 * CKB, 1200 * CKB, 360 * CKB
    codes = [mailbox_bin, ckb_program]
    code_caps = [(len(code) + 100) * CKB for code in codes]
    funding = transaction([vault["change_point"]],
        [output(cap, fixture.standard) for cap in code_caps] +
        [output(mailbox_cap + FEE, fixture.standard), output(authority_funds, ckb_authority),
         output(vault["change"] - sum(code_caps) - mailbox_cap - authority_funds - 2 * FEE, fixture.standard)],
        [hex_bytes(code) for code in codes] + ["0x"] * 3, [secp])
    funded = l1.send(fixture.sign("object-fund-ckb", funding, [wallet_group(0, [fixture.faucet])]))
    type_id = ckb_hash(le(0, 8) + binary(funded) + le(2, 4) + le(0, 8))
    anchor_code, anchor_id = binary(vault["anchor"]["hash"]), vault["anchor"]["id"]
    ckb_deps = [secp, dependency(point(funded, 0)), dependency(point(funded, 1))]
    native_deps = [mint["lock_dep"], point(mint["mint_id"], 2), point(mint["mint_id"], 3)]
    authority_point = point(funded, 3)

    def ckb_submit(signed):
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        return {"hash": tx, "proof": l1.prove_committed(tx), "cycles": cycles,
                "bytes": len(binary(signed["molecule"]))}

    def reject_ckb(signed, code):
        result = l1.reject(signed)
        assert f"error code {code}" in result, result
        return result

    def native_sign(label, spending, outputs, *, endpoint=None, message=None, receive=False, votes=None):
        locks = sorted({packed_script(lock): (lock, key) for _, lock, key in spending}.values(),
                       key=lambda pair: packed_script(pair[0]))
        points = [p for p, _, _ in spending]
        request = {"inputs": [{"out_point": p} for p in points], "outputs": outputs,
                   "cell_deps": [dependency(p) for p in native_deps]}
        if message:
            group = next(i for i, (lock, _) in enumerate(locks) if lock == endpoint)
            request["messages"] = [{"group_index": group, "actions": [
                {"kind": 0xff01 if receive else 0xff00, "data": message["record"]}]}]
        path = args.output / f"object-{label}-request.json"
        draft = args.output / f"object-{label}-draft.json"
        context = args.output / f"object-{label}-context.json"
        write_json(path, request)
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", path)))
        if votes:
            attached = args.output / f"object-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references", "--genesis", spec_path,
                "--transaction", draft, "--observations", votes)))
            draft = attached
        write_json(context, {"cells": [signing_cell(client, p) for p in points + native_deps]})
        for index, (lock, key) in enumerate(locks):
            if key is None:
                continue
            lock_path = args.output / f"object-{label}-lock-{index}.json"
            signed = args.output / f"object-{label}-group-{index}.json"
            write_json(lock_path, lock)
            write_json(signed, json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
                "--transaction", draft, "--context", context, "--group-index", str(index),
                "--lock", lock_path, "--key-file", key)))
            draft = signed
        encoded = json.loads(draft.read_text())
        write_json(args.output / f"object-{label}-stem-signed.json", encoded)
        return encoded

    def commit(tx):
        result = client.call("broadcast_tx_commit", tx=tx)
        assert result["check_tx"]["code"] == 0, result
        return result

    def reject_stem(tx, cause):
        result = client.call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and cause in result["log"], result
        return result["log"]

    def at(tx, index):
        return point("0x" + tx["hash"], index)

    def packed(point_):
        return binary(point_["tx_hash"]) + le(int(point_["index"], 16), 4)

    def query(path, data="0x"):
        reply = client.query(path, data=data, prove=True)
        return reply["height"], json.loads(bytes.fromhex(reply["value"]))

    seed_cap, auth_cap, object_cap, bond_cap = [capacity * CKB for capacity in (450, 200, 200, 120)]
    remainder = mint["payment_capacity"] - seed_cap - auth_cap - FEE
    separated = commit(native_sign("split", [(mint["payment"], owner, owner_key)],
                                   [cell(seed_cap, owner), cell(auth_cap, authority), cell(remainder, owner)]))
    seed, native_authority, native_fee = [at(separated, i) for i in range(3)]
    object_id = ckb_hash(b"Stem object identity\0" + binary(network) + packed(seed) + le(0, 4))
    identity = script(native_object, hex_bytes(object_id + script_hash(authority)
        + authority_id + binary(policy_code)), "data")
    identity_hash = script_hash(identity)
    policy = script(policy_code, hex_bytes(identity_hash + script_hash(authority)), "data")
    types = [identity, policy]
    data = table(molecule_bytes(b"text/plain"), molecule_bytes(b"Stem object with two Type Groups"), b"")
    endpoint = script(native_object, hex_bytes(identity_hash + binary(genesis) + mailbox_code
        + type_id + ckb_code + anchor_code + anchor_id + authority_id), "data")
    endpoint_id = script_hash(endpoint)
    ckb_owner = script(hex_bytes(ckb_code), hex_bytes(type_id + endpoint_id + identity_hash
        + mailbox_code + authority_id), "data2")
    mailbox = script(hex_bytes(mailbox_code), hex_bytes(type_id + binary(genesis) + binary(network)
        + script_hash(ckb_owner) + anchor_code + anchor_id), "data2")
    mailbox_id = script_hash(mailbox)
    wrapper = script(hex_bytes(ckb_code), hex_bytes(mailbox_id + script_hash(ckb_owner) + authority_id), "data2")
    initial = transaction([point(funded, 2)], [output(mailbox_cap, ckb_owner, mailbox)],
                          [hex_bytes(le(0, 8) + empty)], ckb_deps)
    initial["header_deps"] = [genesis]
    live_mailbox = point(l1.send(fixture.sign("object-create-mailbox", initial,
        [wallet_group(0, [fixture.faucet])])), 0)

    def packet(label, from_stem, recipient, nonce, content=data):
        source, destination, sender, receiver = ((network, genesis, endpoint_id, mailbox_id) if from_stem
                                                 else (genesis, network, mailbox_id, endpoint_id))
        request = args.output / f"object-{label}-message.json"
        write_json(request, dict(source=source, destination=destination, sender=hex_bytes(sender),
            receiver=hex_bytes(receiver), nonce=nonce, payload=hex_bytes(identity_hash + recipient + content)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", request))

    rejected = {}
    auth_input = (native_authority, authority, authority_key)
    issued_outputs = [cell(object_cap, owner, data, types), cell(bond_cap, endpoint),
                      cell(seed_cap - object_cap - bond_cap - FEE, owner), cell(auth_cap, authority)]
    stripped_issue = [cell(object_cap, owner, data, [identity]), *issued_outputs[1:]]
    rejected["stem_mint_without_policy_type"] = reject_stem(native_sign("missing-policy",
        [(seed, owner, owner_key), auth_input], stripped_issue), "exited with 117")
    rejected["stem_mint_without_approval"] = reject_stem(native_sign("no-creation-approval",
        [(seed, owner, owner_key)], issued_outputs[:3]), "exited with 115")
    wrong = dict(identity, args=hex_bytes(bytes([7]) * 32 + binary(identity["args"])[32:]))
    wrong_policy = dict(policy, args=hex_bytes(script_hash(wrong) + script_hash(authority)))
    rejected["stem_forged_id"] = reject_stem(native_sign("forged-object", [(seed, owner, owner_key), auth_input],
        [cell(object_cap, owner, data, [wrong, wrong_policy]), *issued_outputs[1:]]), "exited with 117")
    issued = commit(native_sign("issue", [(seed, owner, owner_key), auth_input], issued_outputs))
    wrapped_input = at(issued, 0)
    bond = at(issued, 1)
    native_authority = at(issued, 3)
    out_nonce = int.from_bytes(ckb_hash(b"Stem Spore return nonce\0" + packed(bond))[:8], "little")
    outbound = packet("export", True, script_hash(fixture.owner_lock), out_nonce)
    route_inputs = [(bond, endpoint, None), (wrapped_input, owner, owner_key)]
    route_outputs = [cell(bond_cap, endpoint), cell(object_cap, endpoint, data, types)]
    rejected["stem_export_without_approval"] = reject_stem(native_sign("no-export-approval",
        route_inputs + [(native_fee, owner, owner_key)], route_outputs + [cell(remainder - FEE, owner)],
        endpoint=endpoint, message=outbound), "exited with 115")
    rejected["stem_export_to_mailbox_lock"] = reject_stem(native_sign("export-to-mailbox",
        route_inputs + [(native_authority, authority, authority_key)],
        route_outputs + [cell(auth_cap - FEE, authority)], endpoint=endpoint,
        message=packet("export-to-mailbox", True, script_hash(ckb_owner), out_nonce)), "exited with 118")
    exported = commit(native_sign("export", route_inputs + [(native_authority, authority, authority_key)],
        route_outputs + [cell(auth_cap - FEE, authority)], endpoint=endpoint, message=outbound))
    bond, original, native_authority = [at(exported, i) for i in range(3)]

    proof_height, outbox = query("/stem/intercell/outbox", outbound["id"])
    checkpoint = relay.advance(proof_height, secp, probe=True)
    header, request = [args.output / f"object-outbox-{suffix}.json" for suffix in ("header", "request")]
    write_json(header, hex_bytes(relay.header(proof_height)))
    write_json(request, {**outbox, "consumed": [], "expected_root": hex_bytes(empty)})
    proof = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
    write_json(args.output / "object-ckb-witness.json", proof)
    root = binary(proof["successor_root"])
    path = binary(proof["input_type"])
    next_nonce = 0

    def mint_ckb(label, recipient=fixture.owner_lock, contents=data, *, include_checkpoint=True):
        tx = transaction([live_mailbox, authority_point], [output(mailbox_cap, ckb_owner, mailbox),
            output(token_cap, recipient, wrapper), output(authority_funds - token_cap - FEE, ckb_authority)],
            [hex_bytes(le(next_nonce, 8) + root), hex_bytes(contents), "0x"],
            ckb_deps + ([checkpoint] if include_checkpoint else []))
        tx["witnesses"] = [witness(path)]
        return fixture.sign(f"object-{label}", tx, [wallet_group(1, [authority_key])])

    rejected["ckb_mint_wrong_recipient"] = reject_ckb(mint_ckb("redirect-mint", recipient=fixture.friend_lock), 123)
    rejected["ckb_mint_changed_data"] = reject_ckb(mint_ckb("replace-content", contents=data[:-1] + b"!"), 123)
    rejected["ckb_mint_without_checkpoint"] = reject_ckb(mint_ckb("no-checkpoint", include_checkpoint=False), 84)
    ckb_minted = ckb_submit(mint_ckb("mint"))
    live_mailbox, wrapped_point, authority_point = [point(ckb_minted["hash"], i) for i in range(3)]
    authority_funds -= token_cap + FEE

    def transfer(label, authority_consent=True):
        inputs = [wrapped_point]
        outputs = [output(token_cap - (0 if authority_consent else FEE), fixture.friend_lock, wrapper)]
        keys = [wallet_group(0, [fixture.owner])]
        body = [hex_bytes(data)]
        if authority_consent:
            inputs.append(authority_point)
            outputs.append(output(authority_funds - FEE, ckb_authority))
            body.append("0x")
            keys.append(wallet_group(1, [authority_key]))
        return fixture.sign(f"object-{label}", transaction(inputs, outputs, body, ckb_deps), keys)

    rejected["ckb_transfer_without_approval"] = reject_ckb(transfer("no-transfer-approval", False), 121)
    transferred = ckb_submit(transfer("transfer"))
    wrapped_point, authority_point = [point(transferred["hash"], i) for i in range(2)]
    authority_funds -= FEE

    inbound = packet("return", False, script_hash(buyer), 0)
    def return_ckb(label, *, consume=True, message=inbound):
        inputs = [live_mailbox, authority_point] if not consume else [live_mailbox, wrapped_point, authority_point]
        outputs = [output(mailbox_cap, ckb_owner, mailbox)]
        data_items = [hex_bytes(le(1, 8) + root + binary(message["record"]))]
        keys = [wallet_group(len(inputs) - 1, [authority_key])]
        if consume:
            outputs.append(output(token_cap, fixture.friend_lock))
            data_items.append("0x")
            keys.append(wallet_group(1, [fixture.friend]))
        outputs.append(output(authority_funds - FEE, ckb_authority))
        data_items.append("0x")
        return fixture.sign(f"object-{label}", transaction(inputs, outputs, data_items, ckb_deps), keys)

    rejected["ckb_return_without_burn"] = reject_ckb(return_ckb("return-without-burn", consume=False), 123)
    rejected["ckb_return_to_escrow_lock"] = reject_ckb(return_ckb("return-to-escrow",
        message=packet("return-to-escrow", False, endpoint_id, 0)), 123)
    burned = ckb_submit(return_ckb("burn"))
    live_mailbox, authority_point = [point(burned["hash"], i) for i in (0, 2)]
    authority_funds -= FEE
    next_nonce = 1
    rejected["ckb_replay_after_burn"] = reject_ckb(mint_ckb("repeat-after-burn"), 85)
    assert l1.call("get_live_cell", wrapped_point, True)["status"] != "live"

    def attest(command, label, params):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            votes.append(json.loads(run(args.binary, "bridge", command, "--genesis", spec_path,
                "--l1-rpc", l1.url, "--key-file", key, "--journal", args.output / f"object-{label}-{index}", *params)))
        approval = args.output / f"object-{label}-approvals.json"
        write_json(approval, votes)
        return approval

    l1.mine(spec["bridge"]["confirmations"])
    _, previous = query("/stem/bridge-anchor")
    anchor = burned["proof"]["header"]
    height = int(anchor["number"], 16)
    approvals = attest("attest-anchor", "anchor", ["--previous-height", str(previous["height"]),
        "--previous-hash", previous["hash"], "--height", str(height)])
    advanced = commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", approvals)))
    observation = args.output / "object-reference-request.json"
    write_json(observation, {"out_point": live_mailbox, "anchor_height": height,
                             "anchor_hash": anchor["hash"], "live_at_anchor": False})
    certified = attest("attest-reference", "return", ["--request", observation])

    release_inputs = [(bond, endpoint, None), (original, endpoint, None)]
    release = [cell(bond_cap, endpoint), cell(object_cap, buyer, data, types)]
    with_approval = release_inputs + [(native_authority, authority, authority_key)]
    rejected["stem_return_without_policy_approval"] = reject_stem(native_sign("return-no-consent",
        release_inputs + [(native_fee, owner, owner_key)], release + [cell(remainder - FEE, owner)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified), "exited with 115")
    rejected["stem_return_policy_removed"] = reject_stem(native_sign("return-without-policy",
        with_approval, [release[0], cell(object_cap, buyer, data, [identity]), cell(auth_cap - 2 * FEE, authority)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified), "exited with 117")
    redeemed = commit(native_sign("redeem", with_approval, release + [cell(auth_cap - 2 * FEE, authority)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified))
    restored = signing_cell(client, at(redeemed, 1))
    assert restored["cell"] == release[1]
    reject_query = native_sign("repeat-release",
        [(at(redeemed, 0), endpoint, None), (at(redeemed, 1), buyer, buyer_key),
         (at(redeemed, 2), authority, authority_key)], release + [cell(auth_cap - 3 * FEE, authority)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified)
    rejected["stem_replay_release"] = reject_stem(reject_query, "nonce already used")

    def execution(committed):
        location = client.locations[committed["hash"]]
        response = client.query("/stem/execution", height=location["height"],
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return json.loads(bytes.fromhex(response["value"]))

    return {"id": hex_bytes(object_id), "identity_type": identity, "policy_type": policy, "wrapper_type": wrapper,
            "escrow": endpoint, "mailbox": mailbox, "issued": issued, "exported": exported,
            "ckb_minted": ckb_minted, "ckb_transferred": transferred, "ckb_burned": burned,
            "anchor": advanced, "redeemed": redeemed, "restored": restored,
            "outbox": outbox, "inbox": query("/stem/intercell/inbox", inbound["id"])[1],
            "execution": {name: execution(tx) for name, tx in (("issued", issued), ("exported", exported), ("redeemed", redeemed))},
            "rejections": rejected}

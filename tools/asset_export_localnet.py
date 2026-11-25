"""Real CKB mint and return of a Stem-native application UDT."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell
from stake_localnet import witness


def asset_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    network, genesis = client.network, vault["genesis"]
    native_code = ckb_hash(args.native_lock.read_bytes())
    owner = script(hex_bytes(native_code), fixture.key(fixture.owner)["blake160"], "data")
    beneficiary = script(hex_bytes(native_code), fixture.key(fixture.friend)["blake160"], "data")
    asset = script(hex_bytes(ckb_hash(args.export_udt.read_bytes())),
                   hex_bytes(script_hash(owner) + ckb_hash(b"test UDT series")), "data")
    asset_id = script_hash(asset)
    secondary = dict(asset, args=hex_bytes(script_hash(owner) + ckb_hash(b"independent type")))
    escrow_bin = ckb_hash(args.asset_escrow.read_bytes())
    mailbox_bin = args.intercell_mailbox.read_bytes()
    wrapped_bin = args.ckb_stem_udt.read_bytes()
    mailbox_code = ckb_hash(mailbox_bin)
    wrapped_code = ckb_hash(wrapped_bin)
    empty = bytes(32)
    mailbox_capacity, fee_capacity = 1000 * CKB, 9000 * CKB
    secp = vault["secp"]
    deploy_cap = [(len(b) + 100) * CKB for b in (mailbox_bin, wrapped_bin)]
    fund = transaction([vault["change_point"]],
        [output(cap, fixture.standard) for cap in deploy_cap]
        + [output(mailbox_capacity, fixture.standard), output(fee_capacity, fixture.standard),
           output(vault["change"] - sum(deploy_cap) - mailbox_capacity - fee_capacity - FEE, fixture.standard)],
        [hex_bytes(mailbox_bin), hex_bytes(wrapped_bin), "0x", "0x", "0x"], [secp])
    funded = l1.send(fixture.sign("udt-deploy-code", fund, [wallet_group(0, [fixture.faucet])]))
    type_id = ckb_hash(le(0, 8) + binary(funded) + le(2, 4) + le(0, 8))
    anchor_code = binary(vault["anchor"]["hash"])
    anchor_id = vault["anchor"]["id"]
    escrow = script(hex_bytes(escrow_bin),
        hex_bytes(asset_id + binary(genesis) + mailbox_code + type_id + wrapped_code + anchor_code + anchor_id), "data")
    escrow_id = script_hash(escrow)
    mailbox_lock = script(hex_bytes(wrapped_code), hex_bytes(type_id + escrow_id + asset_id + mailbox_code), "data2")
    mailbox_type = script(hex_bytes(mailbox_code), hex_bytes(type_id + binary(genesis) + binary(network)
                           + script_hash(mailbox_lock) + anchor_code + anchor_id), "data2")
    mailbox_id = script_hash(mailbox_type)
    wrapper = script(hex_bytes(wrapped_code), hex_bytes(mailbox_id), "data2")
    mailbox_point = point(funded, 2)
    fee_point = point(funded, 3)
    mailbox_capacity -= FEE
    deps = [secp, dependency(point(funded, 0)), dependency(point(funded, 1))]
    initial = transaction([mailbox_point], [output(mailbox_capacity, mailbox_lock, mailbox_type)],
                          [hex_bytes(le(0, 8) + empty)], deps)
    initial["header_deps"] = [genesis]
    created = l1.send(fixture.sign("udt-create-mailbox", initial, [wallet_group(0, [fixture.faucet])]))
    mailbox_point = point(created, 0)

    program_points = [mint["lock_dep"], point(mint["mint_id"], 2), point(mint["mint_id"], 3)]
    def stem_signed(label, funding, outputs, *, held=None, msg=None, receive=False, observations=None):
        points = [funding] if held is None else [funding, held]
        locks = [owner] if held is None else sorted((owner, escrow), key=packed_script)
        request = {"inputs": [{"out_point": p} for p in points], "outputs": outputs,
                   "cell_deps": [dependency(p) for p in program_points]}
        if msg:
            request["messages"] = [{"group_index": locks.index(escrow), "actions": [
                {"kind": 0xff01 if receive else 0xff00, "data": msg["record"]}]}]
        request_path, draft, context, lock = (args.output / f"udt-{label}-{kind}.json"
                                              for kind in ("wallet", "draft", "context", "lock"))
        write_json(request_path, request)
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request_path)))
        if observations:
            attached = args.output / f"udt-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references",
                "--genesis", spec_path, "--transaction", draft, "--observations", observations)))
            draft = attached
        write_json(context, {"cells": [signing_cell(client, p) for p in points + program_points]})
        write_json(lock, owner)
        encoded = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
            "--transaction", draft, "--context", context, "--group-index", str(locks.index(owner)),
            "--lock", lock, "--key-file", fixture.owner))
        write_json(args.output / f"udt-{label}-stem-signed.json", encoded)
        return encoded

    def stem_commit(tx):
        result = client.call("broadcast_tx_commit", tx=tx)
        assert result["check_tx"]["code"] == 0, result
        return result

    def stem_reject(tx, reason):
        result = client.call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and reason in result["log"], result
        return result["log"]

    def ckb_reject(signed, code):
        result = l1.reject(signed)
        assert f"error code {code}" in result, result
        return result

    def submit_ckb(signed):
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        return {"hash": tx, "inclusion": l1.prove_committed(tx), "cycles": cycles,
                "bytes": len(binary(signed["molecule"]))}

    def stem_point(commit, index):
        return point("0x" + commit["hash"], index)

    def packet(label, from_stem, nonce, recipient, units):
        source, destination, sender, receiver = ((network, genesis, escrow_id, mailbox_id) if from_stem
                                                 else (genesis, network, mailbox_id, escrow_id))
        payload = asset_id + recipient + le(units, 16)
        path = args.output / f"udt-{label}-message.json"
        write_json(path, dict(source=source, destination=destination, sender=hex_bytes(sender),
                              receiver=hex_bytes(receiver), nonce=nonce, payload=hex_bytes(payload)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", path))

    def query(path, data="0x"):
        proof = client.query(path, data=data, prove=True)
        return proof["height"], json.loads(bytes.fromhex(proof["value"]))

    bond, composite_cap = 100 * CKB, 200 * CKB
    owner_amount = mint["payment_capacity"] - bond - composite_cap - FEE
    issued = stem_commit(stem_signed("issue", mint["payment"], [
        cell(owner_amount, owner, le(700, 16), [asset]), cell(bond, escrow),
        cell(composite_cap, owner, le(300, 16), [asset, secondary])]))
    issue_cell, escrow_cell, composite = (stem_point(issued, i) for i in range(3))
    ckb_recipient = script_hash(fixture.owner_lock)
    packed = binary(escrow_cell["tx_hash"]) + le(int(escrow_cell["index"], 16), 4)
    nonce = int.from_bytes(ckb_hash(b"Stem UDT export nonce\0" + packed)[:8], "little")
    message = packet("export", True, nonce, ckb_recipient, 250)
    remaining_cap = owner_amount - FEE
    rejects = {}
    outputs = [cell(remaining_cap, owner, le(450, 16), [asset]), cell(bond, escrow, le(250, 16), [asset])]
    altered = packet("wrong-export-amount", True, nonce, ckb_recipient, 251)
    rejects["stem_fake_escrow_increase"] = stem_reject(
        stem_signed("fake-export", issue_cell, outputs, held=escrow_cell, msg=altered), "exited with 98")
    arbitrary = packet("wrong-export-nonce", True, nonce ^ 1, ckb_recipient, 250)
    rejects["stem_unbound_export_nonce"] = stem_reject(
        stem_signed("unbound-nonce", issue_cell, outputs, held=escrow_cell, msg=arbitrary), "exited with 97")
    sink = packet("export-to-escrow", True, nonce, script_hash(mailbox_lock), 250)
    rejects["stem_export_to_mailbox_lock"] = stem_reject(
        stem_signed("redirect-to-mailbox", issue_cell, outputs, held=escrow_cell, msg=sink), "exited with 97")
    rejects["stem_composite_export"] = stem_reject(
        stem_signed("composite-export", composite, [
            cell(composite_cap - FEE, owner, le(50, 16), [asset, secondary]),
            cell(bond, escrow, le(250, 16), [asset, secondary])], held=escrow_cell, msg=message),
        "exited with 98")
    rejects["stem_missing_message"] = stem_reject(
        stem_signed("missing-message", issue_cell, outputs, held=escrow_cell), "exited with 96")
    exported = stem_commit(stem_signed("export", issue_cell, outputs, held=escrow_cell, msg=message))
    native_change, native_held = stem_point(exported, 0), stem_point(exported, 1)

    height, proof = query("/stem/intercell/outbox", message["id"])
    checkpoint = relay.advance(height, secp, probe=True)
    header, request = [args.output / f"udt-outbox-{suffix}.json" for suffix in ("header", "proof")]
    write_json(header, hex_bytes(relay.header(height)))
    write_json(request, {**proof, "consumed": [], "expected_root": hex_bytes(empty)})
    ready = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
    write_json(args.output / "udt-ckb-delivery-witness.json", ready)
    root = binary(ready["successor_root"])
    token_capacity = 250 * CKB

    def mint_ckb(label, units, recipient, *, drain=0, payload=None):
        tx = transaction([mailbox_point, fee_point],
            [output(mailbox_capacity - drain, mailbox_lock, mailbox_type),
             output(token_capacity, recipient, wrapper),
             output(fee_capacity - token_capacity - FEE + drain, fixture.standard)],
            [hex_bytes(le(0, 8) + root), hex_bytes(le(units, 16)), "0x"], deps + [checkpoint])
        tx["witnesses"] = [witness(payload if payload is not None else binary(ready["input_type"]))]
        return fixture.sign(f"udt-{label}", tx, [wallet_group(1, [fixture.faucet])])

    naked = transaction([fee_point], [output(token_capacity, fixture.owner_lock, wrapper),
        output(fee_capacity - token_capacity - FEE, fixture.standard)], [hex_bytes(le(250, 16)), "0x"], deps)
    rejects["ckb_unbacked_direct_mint"] = ckb_reject(
        fixture.sign("udt-no-mailbox", naked, [wallet_group(0, [fixture.faucet])]), 103)
    rejects["ckb_inflated_mint"] = ckb_reject(mint_ckb("bad-mint-amount", 251, fixture.owner_lock), 104)
    rejects["ckb_redirect_mint"] = ckb_reject(mint_ckb("bad-mint-recipient", 250, fixture.friend_lock), 104)
    rejects["ckb_mailbox_bond_drain"] = ckb_reject(mint_ckb("drain-mailbox", 250, fixture.owner_lock, drain=CKB), 101)
    issued_wrapped = submit_ckb(mint_ckb("mint", 250, fixture.owner_lock))
    mailbox_point, wrapped_point, fee_point = [point(issued_wrapped["hash"], i) for i in range(3)]
    fee_capacity -= token_capacity + FEE
    rejects["ckb_replay_mint"] = ckb_reject(mint_ckb("replay-mint", 250, fixture.owner_lock), 85)
    transfer = transaction([wrapped_point], [output(token_capacity - FEE, fixture.friend_lock, wrapper)],
                           [hex_bytes(le(250, 16))], deps)
    transferred = submit_ckb(fixture.sign("udt-transfer", transfer, [wallet_group(0, [fixture.owner])]))
    wrapped_point = point(transferred["hash"], 0)
    token_capacity -= FEE

    returned_units = 250 if args.full_udt_return else 150
    retained_units = 250 - returned_units
    returned = packet("return", False, 0, script_hash(beneficiary), returned_units)
    def return_ckb(label, remaining, packet_, use_wrapped=True):
        inputs = [mailbox_point, wrapped_point, fee_point] if use_wrapped else [mailbox_point, fee_point]
        outputs = [output(mailbox_capacity, mailbox_lock, mailbox_type)]
        data = [hex_bytes(le(1, 8) + root + binary(packet_["record"]))]
        if use_wrapped and remaining:
            outputs.append(output(token_capacity, fixture.friend_lock, wrapper))
            data.append(hex_bytes(le(remaining, 16)))
        if use_wrapped and not remaining:
            outputs.append(output(token_capacity, fixture.friend_lock))
            data.append("0x")
        outputs.append(output(fee_capacity - FEE, fixture.standard))
        keys = [wallet_group(1, [fixture.friend]), wallet_group(2, [fixture.faucet])] if use_wrapped else [
            wallet_group(1, [fixture.faucet])]
        return fixture.sign(f"udt-{label}", transaction(inputs, outputs, data + ["0x"], deps), keys)

    rejects["ckb_return_without_burn"] = ckb_reject(return_ckb("missing-burn", 0, returned, False), 104)
    rejects["ckb_return_underburn"] = ckb_reject(return_ckb("underburn", retained_units + 1, returned), 104)
    sink = packet("return-to-escrow", False, 0, escrow_id, returned_units)
    rejects["ckb_return_to_escrow_lock"] = ckb_reject(return_ckb("return-to-escrow", retained_units, sink), 104)
    burned = submit_ckb(return_ckb("burn", retained_units, returned))
    l1.mine(spec["bridge"]["confirmations"])

    _, previous = query("/stem/bridge-anchor")
    l1_header = burned["inclusion"]["header"]
    height, hash_ = int(l1_header["number"], 16), l1_header["hash"]
    def attest(command, suffix, journal, params):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            votes.append(json.loads(run(args.binary, "bridge", command, "--genesis", spec_path,
                "--l1-rpc", l1.url, "--key-file", key, "--journal",
                args.output / f"udt-{journal}-{index}", *params)))
        path = args.output / f"udt-{suffix}-approvals.json"
        write_json(path, votes)
        return path

    votes = attest("attest-anchor", "anchor", "anchor", ["--previous-height", str(previous["height"]),
        "--previous-hash", previous["hash"], "--height", str(height)])
    advance = stem_commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", votes)))
    path = args.output / "udt-return-reference.json"
    write_json(path, {"out_point": point(burned["hash"], 0), "anchor_height": height,
                      "anchor_hash": hash_, "live_at_anchor": False})
    approval = attest("attest-reference", "return", "reference", ["--request", path])

    # Split the circulating coin so a separate fee Cell can fund return without
    # participating in the asset Type Group.
    fee_cell_cap = 100 * CKB
    split = stem_commit(stem_signed("split", native_change, [
        cell(remaining_cap - fee_cell_cap * 2 - FEE, owner, le(450, 16), [asset]),
        cell(fee_cell_cap, owner), cell(fee_cell_cap, owner)]))
    redeem_in = stem_point(split, 1)
    release_cap = 50 * CKB
    change_cell = cell(fee_cell_cap - release_cap - FEE, owner)
    def custody(capacity):
        return (cell(capacity, escrow, le(retained_units, 16), [asset])
                if retained_units else cell(capacity, escrow))
    new_escrow = custody(bond)
    rejects["stem_redirect_return"] = stem_reject(stem_signed("redirect-return", redeem_in,
        [new_escrow, cell(release_cap, owner, le(returned_units, 16), [asset]), change_cell],
        held=native_held, msg=returned, receive=True, observations=approval), "exited with 98")
    rejects["stem_escrow_bond_drain"] = stem_reject(stem_signed("drain-return", redeem_in,
        [custody(bond - CKB), cell(release_cap + CKB, beneficiary, le(returned_units, 16), [asset]), change_cell],
        held=native_held, msg=returned, receive=True, observations=approval), "exited with 98")
    redeemed = stem_commit(stem_signed("redeem", redeem_in, [
        new_escrow, cell(release_cap, beneficiary, le(returned_units, 16), [asset]), change_cell],
        held=native_held, msg=returned, receive=True, observations=approval))
    rejects["stem_replay_return"] = stem_reject(stem_signed("replay-return", stem_point(split, 2),
        [new_escrow, cell(release_cap, beneficiary, le(returned_units, 16), [asset]), change_cell],
        held=stem_point(redeemed, 0), msg=returned, receive=True, observations=approval), "nonce already used")

    def execution(committed):
        location = client.locations[committed["hash"]]
        response = client.query("/stem/execution", height=location["height"],
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return json.loads(bytes.fromhex(response["value"]))

    state = {"escrow": signing_cell(client, stem_point(redeemed, 0)),
             "released": signing_cell(client, stem_point(redeemed, 1)),
             "circulating": signing_cell(client, stem_point(split, 0)),
             "composite": signing_cell(client, composite),
             "wrapped": l1.call("get_live_cell", point(burned["hash"], 1), True)["cell"]}
    for key, expected in (("released", returned_units), ("circulating", 450), ("composite", 300)):
        assert binary(state[key]["cell"]).endswith(le(expected, 16))
    assert state["escrow"]["cell"] == new_escrow
    assert binary(state["wrapped"]["data"]["content"]) == (le(retained_units, 16) if retained_units else b"")
    assert state["wrapped"]["output"]["type"] == (wrapper if retained_units else None)
    return {"gross_export": 250, "returned": returned_units, "outstanding": retained_units, "issue": issued, "export": exported, "redeem": redeemed, "l1_issue": issued_wrapped,
            "l1_burn": burned, "l1_transfer": transferred, "l1_anchor": advance,
            "execution": {name: execution(tx) for name, tx in (("issued", issued), ("exported", exported), ("redeemed", redeemed))},
 "mailbox": mailbox_type, "mailbox_create": l1.prove_committed(created),
            "asset": asset, "wrapped_type": wrapper, "escrow": escrow, "state": state, "rejects": rejects,
            "outbox": proof, "inbox": query("/stem/intercell/inbox", returned["id"])[1]}

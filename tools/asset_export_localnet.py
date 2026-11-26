"""Real CKB mint and return of a Stem-native application UDT."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, l1_reference_request, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from metatype_localnet import capacity as occupied, manifest_definition, metatype_data, metatype_reference
from native_bridge_localnet import cell, signing_cell
from stake_localnet import witness


def asset_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    network, genesis = client.network, vault["genesis"]
    native_code = ckb_hash(args.native_lock.read_bytes())
    owner = script(hex_bytes(native_code), fixture.key(fixture.owner)["blake160"], "data")
    beneficiary = script(hex_bytes(native_code), fixture.key(fixture.friend)["blake160"], "data")
    portable = args.metatype_udt_adapter is not None
    limit = 200
    identity_args = script_hash(owner) + ckb_hash(b"test UDT series")
    if portable:
        policy_code = ckb_hash(args.native_amount_cap.read_bytes())
        identity_args += policy_code + le(limit, 16)
    asset = script(hex_bytes(ckb_hash(args.export_udt.read_bytes())), hex_bytes(identity_args), "data")
    asset_id = script_hash(asset)
    types = [asset]
    if portable:
        policy = script(hex_bytes(policy_code), hex_bytes(asset_id + le(limit, 16)), "data")
        types.append(policy)
    secondary = dict(asset, args=hex_bytes(script_hash(owner) + ckb_hash(b"independent type")))
    escrow_file = args.stem_metatype_escrow if portable else args.asset_escrow
    guard_file = args.metatype_mailbox_lock if portable else args.ckb_stem_udt
    escrow_bin = ckb_hash(escrow_file.read_bytes())
    programs = [args.intercell_mailbox.read_bytes(), guard_file.read_bytes()]
    mailbox_code, guard_code = map(ckb_hash, programs)
    if portable:
        programs.extend(path.read_bytes() for path in
                        (args.metatype_dispatcher, args.metatype_udt_adapter, args.metatype_amount_cap_adapter))
    empty = bytes(32)
    mailbox_capacity, fee_capacity = 1000 * CKB, 9000 * CKB
    secp = vault["secp"]
    deploy_cap = [(len(code) + 100) * CKB for code in programs]
    reserve = vault["change"] - sum(deploy_cap) - mailbox_capacity - fee_capacity - FEE
    fund = transaction([vault["change_point"]],
        [output(cap, fixture.standard) for cap in [*deploy_cap, mailbox_capacity, fee_capacity, reserve]],
        [hex_bytes(code) for code in programs] + ["0x"] * 3, [secp])
    funded = l1.send(fixture.sign("udt-deploy-code", fund, [wallet_group(0, [fixture.faucet])]))
    seed_index = len(programs)
    type_id = ckb_hash(le(0, 8) + binary(funded) + le(seed_index, 4) + le(0, 8))
    anchor_code = binary(vault["anchor"]["hash"])
    anchor_id = vault["anchor"]["id"]
    deps = [secp, *(dependency(point(funded, i)) for i in range(len(programs)))]
    asset_script = packed_script(asset)
    definition = None
    definition_dep = None
    metadata_publication = None
    if portable:
        scripts = sorted(map(packed_script, types))
        wire_asset = ckb_hash(b"Stem Metatype manifest\0" + b"".join(map(ckb_hash, scripts)))
        custody = bytes([2]) + mailbox_code + type_id + guard_code + wire_asset + asset_id
        adapter_entries = sorted([binary(asset["code_hash"]) + bytes([0]) + ckb_hash(programs[3]),
                                  policy_code + bytes([0]) + ckb_hash(programs[4])])
        adapters = bytes([len(adapter_entries)]) + b"".join(adapter_entries) + custody
        published = [adapters]
        if args.metatype_shared_manifest:
            definition = manifest_definition([(logical, b"") for logical in scripts])
            published.append(definition)
        capacities = [(61 + len(data)) * CKB for data in published]
        publication = transaction([point(funded, seed_index + 2)],
            [output(cap, fixture.standard) for cap in [*capacities, reserve - sum(capacities) - FEE]],
            [hex_bytes(data) for data in published] + ["0x"], [secp])
        dep_tx = l1.send(fixture.sign("udt-deploy-manifest", publication, [wallet_group(0, [fixture.faucet])]))
        deps.extend(dependency(point(dep_tx, i)) for i in range(len(published)))
        if definition is not None:
            definition_dep = deps[-1]
        metadata_publication = {
            "proof": l1.prove_committed(dep_tx),
            "adapter_table": hex_bytes(adapters),
            "definition": hex_bytes(definition) if definition is not None else None,
            "definition_occupied": capacities[1] if definition is not None else 0,
        }
        wrapper = script(hex_bytes(ckb_hash(programs[2])), hex_bytes(binary(genesis)
            + ckb_hash(b"Stem fungible Metatype\0" + wire_asset) + ckb_hash(adapters)), "data2")
        wrapper_id = script_hash(wrapper)
        escrow = script(hex_bytes(escrow_bin),
            hex_bytes(binary(genesis) + wrapper_id + anchor_code + anchor_id + custody), "data")
        escrow_id = script_hash(escrow)
        mailbox_lock = script(hex_bytes(guard_code), hex_bytes(bytes([2]) + escrow_id
            + wrapper_id + mailbox_code + type_id + wire_asset + asset_id), "data2")
    else:
        wire_asset = asset_id
        escrow = script(hex_bytes(escrow_bin),
            hex_bytes(asset_id + binary(genesis) + mailbox_code + type_id + guard_code + anchor_code + anchor_id), "data")
        escrow_id = script_hash(escrow)
        mailbox_lock = script(hex_bytes(guard_code), hex_bytes(type_id + escrow_id + asset_id + mailbox_code), "data2")
    mailbox_type = script(hex_bytes(mailbox_code), hex_bytes(type_id + binary(genesis) + binary(network)
                           + script_hash(mailbox_lock) + anchor_code + anchor_id), "data2")
    mailbox_id = script_hash(mailbox_type)
    if not portable:
        wrapper = script(hex_bytes(guard_code), hex_bytes(mailbox_id), "data2")
    mailbox_point = point(funded, seed_index)
    fee_point = point(funded, seed_index + 1)
    mailbox_capacity -= FEE

    def wrapped_data(units, with_policy=True):
        if not portable:
            return le(units, 16)
        components = [(asset_script, le(units, 16))]
        if with_policy:
            components.append((packed_script(policy), b""))
            if definition is not None:
                return metatype_reference(components, definition)
        return metatype_data(components)

    def ckb_tx(inputs, outputs, data, dependencies):
        tx = transaction(inputs, outputs, data, dependencies)
        if portable:
            tx["header_deps"] = [genesis]
        return tx

    initial = transaction([mailbox_point], [output(mailbox_capacity, mailbox_lock, mailbox_type)],
                          [hex_bytes(le(0, 8) + empty)], deps)
    initial["header_deps"] = [genesis]
    created = l1.send(fixture.sign("udt-create-mailbox", initial, [wallet_group(0, [fixture.faucet])]))
    mailbox_point = point(created, 0)

    program_points = [mint["lock_dep"], *(point(mint["mint_id"], index) for index in range(2, 5 if portable else 4))]
    def stem_signed(label, funding, outputs, *, held=None, msg=None, receive=False, observations=None):
        points = list(funding) if isinstance(funding, list) else [funding]
        if held is not None:
            points.extend(held if isinstance(held, list) else [held])
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

    def ckb_reject(signed, *codes):
        result = l1.reject(signed)
        assert any(f"error code {code} " in result for code in codes), result
        return result

    def submit_ckb(signed):
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        return {"hash": tx, "inclusion": l1.prove_committed(tx), "cycles": cycles,
                "bytes": len(binary(signed["molecule"])),
                "witness_bytes": sum(len(binary(value)) for value in signed["json"]["witnesses"])}

    def stem_point(commit, index):
        return point("0x" + commit["hash"], index)

    def packet(label, from_stem, nonce, recipient, units):
        source, destination, sender, receiver = ((network, genesis, escrow_id, mailbox_id) if from_stem
                                                 else (genesis, network, mailbox_id, escrow_id))
        payload = wire_asset + recipient + le(units, 16)
        path = args.output / f"udt-{label}-message.json"
        write_json(path, dict(source=source, destination=destination, sender=hex_bytes(sender),
                              receiver=hex_bytes(receiver), nonce=nonce, payload=hex_bytes(payload)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", path))

    def query(path, data="0x"):
        proof = client.query(path, data=data, prove=True)
        return proof["height"], json.loads(bytes.fromhex(proof["value"]))

    def split_units(total):
        if portable and total > limit:
            return [total // 2, total - total // 2]
        return [total] if total else []

    def distribute(capacity, count):
        return [capacity // count] * (count - 1) + [capacity - capacity // count * (count - 1)]

    exported_units = 250
    source_units = [200, 200] if portable else [700]
    change_units = sum(source_units) - exported_units
    bond, composite_cap, additional = (200 if portable else 100) * CKB, 200 * CKB, (200 if portable else 0) * CKB
    owner_amount = mint["payment_capacity"] - bond - composite_cap - additional - FEE
    composite_units = 150 if portable else 300
    issue_outputs = [cell(owner_amount, owner, le(source_units[0], 16), types), cell(bond, escrow),
                     cell(composite_cap, owner, le(composite_units, 16), [*types, secondary])]
    if portable:
        issue_outputs.append(cell(additional, owner, le(source_units[1], 16), types))
    issued = stem_commit(stem_signed("issue", mint["payment"], issue_outputs))
    issue_cell, escrow_cell, composite = (stem_point(issued, i) for i in range(3))
    funding = [issue_cell, stem_point(issued, 3)] if portable else [issue_cell]
    total_capacity = owner_amount + additional
    ckb_recipient = script_hash(fixture.owner_lock)
    packed = binary(escrow_cell["tx_hash"]) + le(int(escrow_cell["index"], 16), 4)
    nonce_domain = b"Stem Metatype escrow nonce\0" if portable else b"Stem UDT export nonce\0"
    nonce = int.from_bytes(ckb_hash(nonce_domain + packed)[:8], "little")
    message = packet("export", True, nonce, ckb_recipient, exported_units)
    remaining_cap = total_capacity - FEE
    rejects = {}
    shape_error, message_error, missing_error = (54, 53, 53) if portable else (98, 97, 96)
    held_units = split_units(exported_units)
    outputs = [cell(remaining_cap, owner, le(change_units, 16), types)]
    outputs.extend(cell(cap, escrow, le(unit, 16), types)
                   for cap, unit in zip(distribute(bond, len(held_units)), held_units, strict=True))
    if portable:
        rejects["stem_cap_removal"] = stem_reject(stem_signed("drop-cap", issue_cell, [
            cell(owner_amount - FEE, owner, le(200, 16), [asset])]), "exited with 92")
        rejects["stem_cap_exceeded"] = stem_reject(stem_signed("merge-above-cap", funding, [
            cell(owner_amount - FEE, owner, le(250, 16), types),
            cell(additional, owner, le(150, 16), types)]), "exited with 55")
    altered = packet("wrong-export-amount", True, nonce, ckb_recipient, 251)
    rejects["stem_fake_escrow_increase"] = stem_reject(
        stem_signed("fake-export", funding, outputs, held=escrow_cell, msg=altered), f"exited with {shape_error}")
    arbitrary = packet("wrong-export-nonce", True, nonce ^ 1, ckb_recipient, exported_units)
    rejects["stem_unbound_export_nonce"] = stem_reject(
        stem_signed("unbound-nonce", funding, outputs, held=escrow_cell, msg=arbitrary), f"exited with {message_error}")
    sink = packet("export-to-escrow", True, nonce, script_hash(mailbox_lock), exported_units)
    rejects["stem_export_to_mailbox_lock"] = stem_reject(
        stem_signed("redirect-to-mailbox", funding, outputs, held=escrow_cell, msg=sink), f"exited with {message_error}")
    composite_export = composite_units - 50
    composite_message = packet("composite-export", True, nonce, ckb_recipient, composite_export)
    rejects["stem_composite_export"] = stem_reject(
        stem_signed("composite-export", composite, [
            cell(composite_cap - FEE, owner, le(50, 16), [*types, secondary]),
            cell(bond, escrow, le(composite_export, 16), [*types, secondary])],
            held=escrow_cell, msg=composite_message), f"exited with {shape_error}")
    rejects["stem_missing_message"] = stem_reject(
        stem_signed("missing-message", funding, outputs, held=escrow_cell), f"exited with {missing_error}")
    exported = stem_commit(stem_signed("export", funding, outputs, held=escrow_cell, msg=message))
    native_change = stem_point(exported, 0)
    native_held = [stem_point(exported, index) for index in range(1, len(outputs))]

    def cell_proof(outpoint):
        encoded = hex_bytes(binary(outpoint["tx_hash"]) + le(int(outpoint["index"], 16), 4))
        height, proof = query("/stem/cell", encoded)
        return {**proof, "point": encoded, "height": height, "finality": client.finality(height)["proof"]}

    held_proofs = [cell_proof(item) for item in native_held]
    assert all(proof["cell"] and proof["proof"] for proof in held_proofs)
    height, proof = query("/stem/intercell/outbox", message["id"])
    checkpoint = relay.advance(height, secp, probe=True)
    header, request = [args.output / f"udt-outbox-{suffix}.json" for suffix in ("header", "proof")]
    write_json(header, hex_bytes(relay.header(height)))
    write_json(request, {**proof, "consumed": [], "expected_root": hex_bytes(empty)})
    ready = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
    write_json(args.output / "udt-ckb-delivery-witness.json", ready)
    root = binary(ready["successor_root"])
    per_token = (700 if portable else 250) * CKB
    token_capacity = per_token * len(held_units)
    change_error, gate_error = (51, 50) if portable else (104, 101)

    def mint_ckb(label, units, recipient, *, drain=0, omit_definition=False):
        amounts = split_units(units)
        token_outputs = [output(per_token, recipient, wrapper)] * len(amounts)
        used_deps = [dep for dep in deps if dep != definition_dep] if omit_definition else deps
        tx = ckb_tx([mailbox_point, fee_point],
            [output(mailbox_capacity - drain, mailbox_lock, mailbox_type), *token_outputs,
             output(fee_capacity - per_token * len(amounts) - FEE + drain, fixture.standard)],
            [hex_bytes(le(0, 8) + root), *(hex_bytes(wrapped_data(unit)) for unit in amounts), "0x"],
            used_deps + [checkpoint])
        tx["witnesses"] = [witness(binary(ready["input_type"]))]
        return fixture.sign(f"udt-{label}", tx, [wallet_group(1, [fixture.faucet])])

    naked = ckb_tx([fee_point],
        [*[output(per_token, fixture.owner_lock, wrapper)] * len(held_units),
         output(fee_capacity - token_capacity - FEE, fixture.standard)],
        [*(hex_bytes(wrapped_data(unit)) for unit in held_units), "0x"], deps)
    rejects["ckb_unbacked_direct_mint"] = ckb_reject(
        fixture.sign("udt-no-mailbox", naked, [wallet_group(0, [fixture.faucet])]), 48 if portable else 103)
    rejects["ckb_inflated_mint"] = ckb_reject(mint_ckb("bad-mint-amount", 251, fixture.owner_lock), change_error)
    rejects["ckb_redirect_mint"] = ckb_reject(mint_ckb("bad-mint-recipient", exported_units, fixture.friend_lock), change_error)
    rejects["ckb_mailbox_bond_drain"] = ckb_reject(mint_ckb("drain-mailbox", exported_units, fixture.owner_lock, drain=CKB), gate_error)
    if definition_dep is not None:
        rejects["ckb_missing_definition"] = ckb_reject(
            mint_ckb("missing-definition", exported_units, fixture.owner_lock, omit_definition=True), 43, 51)
    issued_wrapped = submit_ckb(mint_ckb("mint", exported_units, fixture.owner_lock))
    mailbox_point = point(issued_wrapped["hash"], 0)
    wrapped_points = [point(issued_wrapped["hash"], index) for index in range(1, 1 + len(held_units))]
    fee_point = point(issued_wrapped["hash"], 1 + len(held_units))
    fee_capacity -= token_capacity + FEE
    rejects["ckb_replay_mint"] = ckb_reject(mint_ckb("replay-mint", exported_units, fixture.owner_lock), 85)
    token_capacity -= FEE

    def move_ckb(label, amounts, *, with_policy=True):
        outputs = [output(cap, fixture.friend_lock, wrapper) for cap in distribute(token_capacity, len(amounts))]
        data = [hex_bytes(wrapped_data(unit, with_policy)) for unit in amounts]
        keys = dict(wallet_group(0, [fixture.owner]), inputs=list(range(len(wrapped_points))))
        return fixture.sign(f"udt-{label}", ckb_tx(wrapped_points, outputs, data, deps), [keys])

    if portable:
        rejects["ckb_cap_exceeded"] = ckb_reject(move_ckb("merge-above-cap", [exported_units]), 46)
        rejects["ckb_cap_removed"] = ckb_reject(move_ckb("remove-cap", held_units, with_policy=False), 48)
    traded_units = [175, exported_units - 175] if portable else [exported_units]
    transferred = submit_ckb(move_ckb("transfer", traded_units))
    wrapped_points = [point(transferred["hash"], index) for index in range(len(traded_units))]

    returned_units = exported_units if args.full_udt_return else 150
    retained_units = exported_units - returned_units
    returned = packet("return", False, 0, script_hash(beneficiary), returned_units)

    def return_ckb(label, remaining, packet_, use_wrapped=True, sequence=0):
        token_inputs = wrapped_points if use_wrapped else []
        inputs = [mailbox_point, *token_inputs, fee_point]
        outputs = [output(mailbox_capacity, mailbox_lock, mailbox_type)]
        data = [hex_bytes(le(sequence + 1, 8) + root + binary(packet_["record"]))]
        if use_wrapped:
            outputs.append(output(token_capacity, fixture.friend_lock, wrapper if remaining else None))
            data.append(hex_bytes(wrapped_data(remaining)) if remaining else "0x")
        outputs.append(output(fee_capacity - FEE, fixture.standard))
        keys = [wallet_group(len(inputs) - 1, [fixture.faucet])]
        if use_wrapped:
            keys.append(dict(wallet_group(1, [fixture.friend]), inputs=list(range(1, len(inputs) - 1))))
        return fixture.sign(f"udt-{label}", ckb_tx(inputs, outputs, data + ["0x"], deps), keys)

    rejects["ckb_return_without_burn"] = ckb_reject(return_ckb("missing-burn", 0, returned, False), change_error)
    rejects["ckb_return_underburn"] = ckb_reject(return_ckb("underburn", retained_units + 1, returned), change_error)
    sink = packet("return-to-escrow", False, 0, escrow_id, returned_units)
    rejects["ckb_return_to_escrow_lock"] = ckb_reject(return_ckb("return-to-escrow", retained_units, sink), change_error)
    burned = submit_ckb(return_ckb("burn", retained_units, returned))

    def attest(command, suffix, journal, params):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            votes.append(json.loads(run(args.binary, "bridge", command, "--genesis", spec_path,
                "--l1-rpc", l1.url, "--key-file", key, "--journal",
                args.output / f"udt-{journal}-{index}", *params)))
        path = args.output / f"udt-{suffix}-approvals.json"
        write_json(path, votes)
        return path

    def authenticate_burn(committed, label):
        l1.mine(spec["bridge"]["confirmations"])
        _, previous = query("/stem/bridge-anchor")
        l1_header = committed["inclusion"]["header"]
        height, hash_ = int(l1_header["number"], 16), l1_header["hash"]
        votes = attest("attest-anchor", f"{label}-anchor", "anchor", ["--previous-height", str(previous["height"]),
            "--previous-hash", previous["hash"], "--height", str(height)])
        advance = stem_commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", votes)))
        path = args.output / f"udt-{label}-reference.json"
        write_json(path, l1_reference_request(height, hash_, (point(committed["hash"], 0), False)))
        approved = attest("attest-reference", label, "reference", ["--request", path])
        return advance, approved

    advance, approval = authenticate_burn(burned, "return")

    # The return consumes a pure fee Cell alongside the escrow inputs.
    fee_cell_cap = 100 * CKB
    split = stem_commit(stem_signed("split", native_change, [
        cell(remaining_cap - fee_cell_cap * 2 - FEE, owner, le(change_units, 16), types),
        cell(fee_cell_cap, owner), cell(fee_cell_cap, owner)]))
    redeem_in = stem_point(split, 1)
    release_cap = 50 * CKB
    change_cell = cell(fee_cell_cap - release_cap - FEE, owner)
    release_units = split_units(returned_units)

    def custody(capacity):
        return (cell(capacity, escrow, le(retained_units, 16), types)
                if retained_units else cell(capacity, escrow))

    def releases(lock, capacity):
        return [cell(cap, lock, le(unit, 16), types)
                for cap, unit in zip(distribute(capacity, len(release_units)), release_units, strict=True)]

    new_escrow = custody(bond)
    rejects["stem_redirect_return"] = stem_reject(stem_signed("redirect-return", redeem_in,
        [new_escrow, *releases(owner, release_cap), change_cell],
        held=native_held, msg=returned, receive=True, observations=approval), f"exited with {shape_error}")
    rejects["stem_escrow_bond_drain"] = stem_reject(stem_signed("drain-return", redeem_in,
        [custody(bond - CKB), *releases(beneficiary, release_cap + CKB), change_cell],
        held=native_held, msg=returned, receive=True, observations=approval), f"exited with {shape_error}")
    release = [new_escrow, *releases(beneficiary, release_cap), change_cell]
    redeemed = stem_commit(stem_signed("redeem", redeem_in, release,
        held=native_held, msg=returned, receive=True, observations=approval))
    rejects["stem_replay_return"] = stem_reject(stem_signed("replay-return", stem_point(split, 2),
        release, held=stem_point(redeemed, 0), msg=returned, receive=True, observations=approval), "nonce already used")

    def execution(committed):
        location = client.locations[committed["hash"]]
        height = location["height"]
        response = client.query("/stem/execution", height=height,
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return {**json.loads(bytes.fromhex(response["value"])), "height": height,
                "finality": client.finality(height)["proof"]}

    released_points = [stem_point(redeemed, index) for index in range(1, 1 + len(release_units))]
    released_cells = [signing_cell(client, item) for item in released_points]
    state = {"escrow": signing_cell(client, stem_point(redeemed, 0)),
             "released": released_cells[0], "released_cells": released_cells,
             "circulating": signing_cell(client, stem_point(split, 0)),
             "composite": signing_cell(client, composite),
             "wrapped": l1.call("get_live_cell", point(burned["hash"], 1), True)["cell"]}
    for key, expected in (("circulating", change_units), ("composite", composite_units)):
        assert binary(state[key]["cell"]).endswith(le(expected, 16))
    assert [item["cell"] for item in released_cells] == release[1:-1]
    assert state["escrow"]["cell"] == new_escrow
    assert binary(state["wrapped"]["data"]["content"]) == (wrapped_data(retained_units) if retained_units else b"")
    assert state["wrapped"]["output"]["type"] == (wrapper if retained_units else None)
    proofs = {"held": held_proofs, "spent": [cell_proof(item) for item in native_held],
              "released": [cell_proof(item) for item in released_points],
              "escrow": cell_proof(stem_point(redeemed, 0))}
    assert all(item["cell"] is None and item["proof"] for item in proofs["spent"])
    assert all(item["cell"] and item["proof"] for item in proofs["released"])
    followup = None
    if args.finish_udt_return:
        mailbox_point, wrapped, fee_point = [point(burned["hash"], index) for index in range(3)]
        wrapped_points = [wrapped]
        fee_capacity -= FEE
        final_packet = packet("final-return", False, 1, script_hash(beneficiary), retained_units)
        stale = packet("stale-nonce", False, 0, script_hash(beneficiary), retained_units)
        rejects["ckb_stale_return_nonce"] = ckb_reject(return_ckb("stale-nonce", 0, stale), 83)
        final_burn = submit_ckb(return_ckb("final-burn", 0, final_packet, sequence=1))
        final_anchor, final_votes = authenticate_burn(final_burn, "final-return")
        final_outputs = [cell(bond, escrow), cell(release_cap, beneficiary, le(retained_units, 16), types), change_cell]
        last_held = stem_point(redeemed, 0)
        final_commit = stem_commit(stem_signed("final-redeem", stem_point(split, 2), final_outputs,
            held=last_held, msg=final_packet, receive=True, observations=final_votes))
        final_proofs = {"spent": cell_proof(last_held), "released": cell_proof(stem_point(final_commit, 1)),
                        "escrow": cell_proof(stem_point(final_commit, 0))}
        assert final_proofs["spent"]["cell"] is None and final_proofs["spent"]["proof"]
        assert signing_cell(client, stem_point(final_commit, 0))["cell"] == final_outputs[0]
        assert signing_cell(client, stem_point(final_commit, 1))["cell"] == final_outputs[1]
        ckb_change = l1.call("get_live_cell", point(final_burn["hash"], 1), True)["cell"]
        assert ckb_change["output"]["type"] is None and ckb_change["data"]["content"] == "0x"
        followup = {"returned": retained_units, "outstanding": 0, "burn": final_burn, "redeem": final_commit,
                    "anchor": final_anchor, "execution": execution(final_commit), "proofs": final_proofs,
                    "inbox": query("/stem/intercell/inbox", final_packet["id"])[1]}
    return {"gross_export": exported_units, "returned": returned_units, "outstanding": retained_units,
            "metatype": portable, "ceiling": limit if portable else None,
            "manifest": hex_bytes(wire_asset), "native_types": types,
            "metadata": metadata_publication, "stem_held_units": held_units, "released_units_by_cell": release_units,
            "ckb_issue_occupied": [occupied(fixture.owner_lock, wrapper, wrapped_data(unit)) for unit in held_units],
            "issue": issued, "export": exported, "redeem": redeemed, "l1_issue": issued_wrapped,
            "l1_burn": burned, "l1_transfer": transferred, "l1_anchor": advance,
            "execution": {name: execution(tx) for name, tx in (("issued", issued), ("exported", exported), ("redeemed", redeemed))},
            "proofs": proofs, "followup": followup, "mailbox": mailbox_type, "mailbox_create": l1.prove_committed(created),
            "asset": asset, "wrapped_type": wrapper, "escrow": escrow, "state": state, "rejects": rejects,
            "outbox": proof, "inbox": query("/stem/intercell/inbox", returned["id"])[1]}

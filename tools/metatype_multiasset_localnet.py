"""Settle two independently capped Metatype assets atomically on both chains."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, l1_reference_request,
                             le, output, packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from metatype_localnet import capacity as occupied, manifest_definition, metatype_data, metatype_reference
from native_bridge_localnet import cell, signing_cell, wait_for
from stake_localnet import witness


def multiasset_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    composed = args.metatype_composed_cell
    partial = args.metatype_composed_partial
    network, genesis = client.network, vault["genesis"]
    native_lock_code = hex_bytes(ckb_hash(args.native_lock.read_bytes()))
    owner, friend = [script(native_lock_code, fixture.key(key)["blake160"], "data")
                     for key in (fixture.owner, fixture.friend)]
    secp = vault["secp"]
    empty = bytes(32)
    code = [path.read_bytes() for path in (args.intercell_mailbox, args.metatype_mailbox_lock,
            args.metatype_dispatcher, args.metatype_udt_adapter, args.metatype_amount_cap_adapter)]
    mailbox_code, gate_code, dispatcher, udt_adapter, cap_adapter = map(ckb_hash, code)
    udt_code = ckb_hash(args.export_udt.read_bytes())
    cap_code = ckb_hash(args.native_amount_cap.read_bytes())
    escrow_code = ckb_hash(args.stem_metatype_escrow.read_bytes())
    checkpoint_code, checkpoint_id = binary(vault["anchor"]["hash"]), vault["anchor"]["id"]
    mailbox_capacity = 1000 * CKB
    token_capacity = (1_000 if composed else 700) * CKB
    bond = release_capacity = native_asset_capacity = 150 * CKB
    fee_capacity = 10_000 * CKB
    assets_by_lane = [(0, 1)] if composed else [(0,), (1,)]
    lane_count = len(assets_by_lane)

    def signed_ckb(label, inputs, outputs, data, keys, *, deps=None, witnesses=None):
        tx = transaction(inputs, outputs, data, dependencies if deps is None else deps)
        tx["header_deps"] = [genesis]
        if witnesses is not None:
            tx["witnesses"] = witnesses
        return fixture.sign(f"metatype-batch-{label}", tx, keys)

    def submit_ckb(signed):
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        return {"hash": tx, "proof": l1.prove_committed(tx), "cycles": cycles,
                "bytes": len(binary(signed["molecule"])),
                "witness_bytes": sum(len(binary(value)) for value in signed["json"]["witnesses"])}

    def reject_ckb(signed, *codes):
        result = l1.reject(signed)
        assert any(f"error code {code} " in result for code in codes), result
        return result

    deploy_capacity = [(len(program) + 100) * CKB for program in code]
    reserve = vault["change"] - sum(deploy_capacity) - lane_count * mailbox_capacity - fee_capacity - FEE
    funding = signed_ckb("fund", [vault["change_point"]],
        [output(capacity, fixture.standard) for capacity in [
            *deploy_capacity, lane_count * mailbox_capacity, fee_capacity, reserve]],
        [*map(hex_bytes, code), "0x", "0x", "0x"],
        [wallet_group(0, [fixture.faucet])], deps=[secp])
    funded = l1.send(funding)
    seed = point(funded, len(code))
    seed_bytes = binary(funded) + le(len(code), 4)
    fee_point = point(funded, len(code) + 1)
    metadata = []
    lanes = []
    for i, asset_indices in enumerate(assets_by_lane):
        receiver, returning = (fixture.owner_lock, fixture.friend_lock)[i], (friend, owner)[i]
        mailbox_id = ckb_hash(le(0, 8) + seed_bytes + le(i, 8))
        assets = []
        for index in asset_indices:
            units, cap = (160, 250)[index], (200, 300)[index]
            if composed and index == 0:
                cap = 275
            asset = script(hex_bytes(udt_code), hex_bytes(script_hash(owner)
                + ckb_hash(b"Stem atomic Metatype asset" + bytes([index])) + cap_code + le(cap, 16)), "data")
            asset_id = script_hash(asset)
            policy = script(hex_bytes(cap_code), hex_bytes(asset_id + le(cap, 16)), "data")
            assets.append(dict(id=asset_id, units=units, cap=cap, types=[asset, policy]))
        types = [item for asset in assets for item in asset["types"]]
        scripts = sorted(map(packed_script, types))
        manifest = ckb_hash(b"Stem Metatype manifest\0" + b"".join(map(ckb_hash, scripts)))
        profile = bytes([4 if composed else 2])
        ids = sorted(asset["id"] for asset in assets)
        principal = ckb_hash(b"Stem Metatype amount keys\0" + b"".join(ids)) if composed else ids[0]
        custody = profile + mailbox_code + mailbox_id + gate_code + manifest + principal
        table = bytes([2]) + b"".join(sorted((
            udt_code + b"\0" + udt_adapter, cap_code + b"\0" + cap_adapter))) + custody
        metadata.append(table)
        definition = None
        if args.metatype_shared_manifest:
            definition = manifest_definition([(raw, b"") for raw in scripts])
            metadata.append(definition)
        wrapper = script(hex_bytes(dispatcher), hex_bytes(binary(genesis)
            + ckb_hash(b"Stem Metatype batch deployment" + manifest) + ckb_hash(table)), "data2")
        escrow = script(hex_bytes(escrow_code), hex_bytes(binary(genesis)
            + script_hash(wrapper) + checkpoint_code + checkpoint_id + custody), "data")
        gate = script(hex_bytes(gate_code), hex_bytes(profile + script_hash(escrow)
            + script_hash(wrapper) + mailbox_code + mailbox_id + manifest + principal), "data2")
        mailbox = script(hex_bytes(mailbox_code), hex_bytes(mailbox_id + binary(genesis)
            + binary(network) + script_hash(gate) + checkpoint_code + checkpoint_id), "data2")
        lanes.append(dict(assets=assets, types=types, manifest=manifest, definition=definition, receiver=receiver,
                          returning=returning, escrow=escrow, gate=gate, mailbox=mailbox, wrapper=wrapper))
    metadata_cap = [(61 + len(raw)) * CKB for raw in metadata]
    published = l1.send(signed_ckb("publish", [point(funded, len(code) + 2)],
        [output(capacity, fixture.standard) for capacity in [*metadata_cap, reserve - sum(metadata_cap) - FEE]],
        [*map(hex_bytes, metadata), "0x"], [wallet_group(0, [fixture.faucet])], deps=[secp]))
    dependencies = [secp, *(dependency(point(funded, i)) for i in range(len(code))),
                    *(dependency(point(published, i)) for i in range(len(metadata)))]
    mailbox_caps = [mailbox_capacity] * lane_count
    mailbox_caps[-1] -= FEE
    successors = [output(capacity, lane["gate"], lane["mailbox"])
                  for capacity, lane in zip(mailbox_caps, lanes, strict=True)]
    created = l1.send(signed_ckb("create-mailboxes", [seed], successors,
        [hex_bytes(le(0, 8) + empty)] * lane_count, [wallet_group(0, [fixture.faucet])]))
    mailboxes = [point(created, i) for i in range(lane_count)]

    def amounts(lane, units=None):
        return [(asset["id"], asset["units"] if units is None else units[i])
                for i, asset in enumerate(lane["assets"])]

    def amount_data(lane, units=None):
        entries = sorted(amounts(lane, units))
        if not composed:
            return le(entries[0][1], 16)
        return bytes([len(entries)]) + b"".join(asset + le(quantity, 16) for asset, quantity in entries)

    def altered(lane, offset):
        units = [asset["units"] for asset in lane["assets"]]
        units[-1] += offset
        return units

    def representation(lane, units=None, swap_states=False):
        quantities = list(reversed(amounts(lane, units))) if swap_states else amounts(lane, units)
        entries = [(packed_script(asset["types"][0]), le(quantity, 16))
                   for asset, (_, quantity) in zip(lane["assets"], quantities, strict=True)]
        entries.extend((packed_script(asset["types"][1]), b"") for asset in lane["assets"])
        return hex_bytes(metatype_reference(entries, lane["definition"])
                         if lane["definition"] is not None else metatype_data(entries))

    def stem_point(tx, index):
        return point("0x" + tx["hash"], index)

    native_deps = [mint["lock_dep"], *(point(mint["mint_id"], i) for i in range(2, 5))]

    def stem_signed(label, inputs, outputs, *, messages=(), observations=None, signers=None):
        locks = sorted({packed_script(lock) for _, lock in inputs})
        groups = {}
        for lock, action, msg in messages:
            groups.setdefault(locks.index(packed_script(lock)), []).append({"kind": action, "data": msg["record"]})
        request = dict(inputs=[{"out_point": location} for location, _ in inputs], outputs=outputs,
                       cell_deps=[dependency(location) for location in native_deps],
                       messages=[dict(group_index=i, actions=groups[i]) for i in sorted(groups)])
        request_file = args.output / f"metatype-batch-{label}-wallet.json"
        draft = args.output / f"metatype-batch-{label}-draft.json"
        context = args.output / f"metatype-batch-{label}-context.json"
        write_json(request_file, request)
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request_file)))
        if observations:
            attached = args.output / f"metatype-batch-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references",
                "--genesis", spec_path, "--transaction", draft, "--observations", observations)))
            draft = attached
        write_json(context, {"cells": [signing_cell(client, location)
                                      for location in [*(p for p, _ in inputs), *native_deps]]})
        for i, (lock, key) in enumerate(signers or [(owner, fixture.owner)]):
            lock_file = args.output / f"metatype-batch-{label}-lock-{i}.json"
            write_json(lock_file, lock)
            tx = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path, "--transaction", draft,
                "--context", context, "--group-index", str(locks.index(packed_script(lock))),
                "--lock", lock_file, "--key-file", key))
            draft = args.output / f"metatype-batch-{label}-group-{i}.json"
            write_json(draft, tx)
        write_json(args.output / f"metatype-batch-{label}-stem-signed.json", tx)
        return tx

    def commit(tx):
        result = client.call("broadcast_tx_commit", tx=tx)
        assert result.get("check_tx", {}).get("code") == 0, result
        return result

    def reject_stem(tx, *errors):
        result = client.call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and any(error in result["log"] for error in errors), result
        return result["log"]

    def packet(label, lane, outbound, nonce, recipient, units=None):
        route = (network, genesis, lane["escrow"], lane["mailbox"]) if outbound else (
            genesis, network, lane["mailbox"], lane["escrow"])
        source, destination, sender, receiver = route
        payload = lane["manifest"] + script_hash(recipient) + amount_data(lane, units)
        path = args.output / f"metatype-batch-{label}-message.json"
        write_json(path, dict(source=source, destination=destination, sender=hex_bytes(script_hash(sender)),
            receiver=hex_bytes(script_hash(receiver)), nonce=nonce, payload=hex_bytes(payload)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", path))

    def query(path, key="0x"):
        reply = client.query(path, data=key, prove=True)
        return reply["height"], json.loads(bytes.fromhex(reply["value"]))

    def certified(path, key):
        height, proof = query(path, key)
        return {**proof, "height": height, "finality": client.finality(height)["proof"]}

    def cell_proof(location):
        key = hex_bytes(binary(location["tx_hash"]) + le(int(location["index"], 16), 4))
        return {**certified("/stem/cell", key), "point": key}

    def execution(tx):
        location = client.locations[tx["hash"]]
        height = location["height"]
        reply = client.query("/stem/execution", height=height, data=hex_bytes(le(location["index"], 4)), prove=True)
        return {**json.loads(bytes.fromhex(reply["value"])), "height": height,
                "finality": client.finality(height)["proof"]}

    initial_change = mint["payment_capacity"] - (bond + native_asset_capacity) * lane_count - FEE
    if composed:
        asset_outputs = [cell(native_asset_capacity // 2, owner, le(asset["units"], 16), asset["types"])
                         for asset in lanes[0]["assets"]]
    else:
        asset_outputs = [cell(native_asset_capacity, owner, amount_data(lane), lane["types"]) for lane in lanes]
    issue = commit(stem_signed("issue", [(mint["payment"], owner)],
        [*(cell(bond, lane["escrow"]) for lane in lanes), *asset_outputs, cell(initial_change, owner)]))
    rejections = {}
    bond_inputs = [(stem_point(issue, i), lane["escrow"]) for i, lane in enumerate(lanes)]
    bundle = None
    funding = [(stem_point(issue, i), owner) for i in range(lane_count, lane_count + len(asset_outputs))]
    circulating_capacity = lane_count * native_asset_capacity
    if composed:
        lane = lanes[0]
        data = amount_data(lane)
        circulating_capacity -= FEE
        def combine(label, data):
            return stem_signed(label, funding, [cell(circulating_capacity, owner, data, lane["types"])])
        rejections["unordered_native_amounts"] = reject_stem(
            combine("unordered-native-amounts", data[:1] + data[49:] + data[1:49]), "exited with 55", "exited with 92")
        over_cap = altered(lane, lane["assets"][-1]["cap"] - lane["assets"][-1]["units"] + 1)
        rejections["native_component_cap"] = reject_stem(
            combine("native-component-cap", amount_data(lane, over_cap)), "exited with 55")
        combined = commit(combine("combine", data))
        combined_point = stem_point(combined, 0)
        funding = [(combined_point, owner)]
        bundle = {"combined": execution(combined), "combined_cell": cell_proof(combined_point)}
    export_inputs = [*bond_inputs, *funding, (stem_point(issue, lane_count + len(asset_outputs)), owner)]
    export_change = initial_change + circulating_capacity - FEE
    export_outputs = [*(cell(bond, lane["escrow"], amount_data(lane), lane["types"]) for lane in lanes),
                      cell(export_change, owner)]
    outbound = []
    for i, lane in enumerate(lanes):
        packed = binary(bond_inputs[i][0]["tx_hash"]) + le(i, 4)
        nonce = int.from_bytes(ckb_hash(b"Stem Metatype escrow nonce\0" + packed)[:8], "little")
        outbound.append(packet(f"export-{i}", lane, True, nonce, lane["receiver"]))
    sends = [(lane["escrow"], 0xff00, msg) for lane, msg in zip(lanes, outbound, strict=True)]
    rejections["duplicate_lock_action"] = reject_stem(stem_signed("duplicate-lock-action", export_inputs,
        export_outputs, messages=[*sends, sends[0]]), "only one Intercell action is allowed per Lock group")
    rejections["missing_export_action"] = reject_stem(stem_signed("missing-export-action", export_inputs,
        export_outputs, messages=sends[:-1]), "exited with 53")
    outbound_before = [certified("/stem/intercell/outbox", msg["id"]) for msg in outbound]
    assert all(item["record"] is None for item in outbound_before)
    exported = commit(stem_signed("export", export_inputs, export_outputs, messages=sends))
    held = [stem_point(exported, i) for i in range(lane_count)]

    def outbox_snapshot():
        results = [query("/stem/intercell/outbox", msg["id"]) for msg in outbound]
        return results if len({height for height, _ in results}) == 1 else None

    outbox = wait_for(outbox_snapshot, "two outbox proofs at one finalized Stem height")
    height = outbox[0][0]
    checkpoint = relay.advance(height, secp, probe=True)
    header_file = args.output / "metatype-batch-checkpoint-header.json"
    write_json(header_file, hex_bytes(relay.header(height)))
    deliveries = []
    for i, (_, proof) in enumerate(outbox):
        request = args.output / f"metatype-batch-{i}-delivery-request.json"
        write_json(request, {**proof, "consumed": [], "expected_root": hex_bytes(empty)})
        deliveries.append(json.loads(run(args.binary, "intercell", "delivery-witness",
                                        "--request", request, "--header", header_file)))
    roots = [binary(delivery["successor_root"]) for delivery in deliveries]
    mail_input = [witness(binary(delivery["input_type"])) for delivery in deliveries]
    minted_outputs = [*successors,
        *(output(token_capacity, lane["receiver"], lane["wrapper"]) for lane in lanes),
        output(fee_capacity - lane_count * token_capacity - FEE, fixture.standard)]
    minted_data = [*(hex_bytes(le(0, 8) + root) for root in roots),
                   *(representation(lane) for lane in lanes), "0x"]

    def mint_both(label, delivery_witnesses):
        return signed_ckb(label, [*mailboxes, fee_point], minted_outputs, minted_data,
            [wallet_group(lane_count, [fixture.faucet])], deps=[*dependencies, checkpoint], witnesses=delivery_witnesses)

    wrong_witness = [witness(b"")] if composed else [mail_input[0]] * lane_count
    rejections["substituted_mint_witness"] = reject_ckb(mint_both("wrong-mint-witness", wrong_witness), 51, 84)
    assert all(l1.call("get_live_cell", mailbox, False)["status"] == "live" for mailbox in mailboxes)
    minted = submit_ckb(mint_both("mint", mail_input))
    mailboxes = [point(minted["hash"], i) for i in range(lane_count)]
    wrapped = [point(minted["hash"], i) for i in range(lane_count, 2 * lane_count)]
    fee_point = point(minted["hash"], 2 * lane_count)
    fee_capacity -= lane_count * token_capacity + FEE
    holders = [fixture.friend_lock, fixture.owner_lock][:lane_count]
    owners = [fixture.friend, fixture.owner][:lane_count]

    def swap(label, extra_units=0, swap_states=False):
        return signed_ckb(label, wrapped,
            [output(token_capacity - (FEE if i == 0 else 0), lock, lane["wrapper"])
             for i, (lane, lock) in enumerate(zip(lanes, holders, strict=True))],
            [representation(lane, altered(lane, extra_units) if i == lane_count - 1 else None, swap_states)
             for i, lane in enumerate(lanes)],
            [wallet_group(i, [key]) for i, key in enumerate((fixture.owner, fixture.friend)[:lane_count])])

    rejections["swap_inflation"] = reject_ckb(swap("swap-inflation", 1), 48)
    if composed:
        rejections["swapped_amount_keys"] = reject_ckb(swap("swap-amount-keys", swap_states=True), 48)
    assert all(l1.call("get_live_cell", location, False)["status"] == "live" for location in wrapped)
    swapped = submit_ckb(swap("swap"))
    wrapped = [point(swapped["hash"], i) for i in range(lane_count)]
    assert [l1.call("get_live_cell", item, True)["cell"]["output"]["lock"] for item in wrapped] == holders
    returning = [[asset["units"] for asset in lane["assets"]] for lane in lanes]
    if partial:
        returning[0] = [60, 110]
    remaining = [[asset["units"] - amount for asset, amount in zip(lane["assets"], quantities, strict=True)]
                 for lane, quantities in zip(lanes, returning, strict=True)]
    inbound = [packet(f"return-{i}", lane, False, 0, lane["returning"], returning[i]) for i, lane in enumerate(lanes)]
    wrong_amount = returning[-1].copy()
    wrong_amount[-1] -= 1
    wrong = packet("wrong-burn-amount", lanes[-1], False, 0, lanes[-1]["returning"], wrong_amount)

    def burn_both(label, packets, retained=remaining):
        retained_outputs = [output(token_capacity - (FEE if i == 0 else 0), lock,
                                   lane["wrapper"] if any(retained[i]) else None)
                            for i, (lock, lane) in enumerate(zip(holders, lanes, strict=True))]
        retained_data = [representation(lane, retained[i]) if any(retained[i]) else "0x"
                         for i, lane in enumerate(lanes)]
        return signed_ckb(label, [*mailboxes, *wrapped, fee_point],
            [*successors, *retained_outputs, output(fee_capacity - FEE, fixture.standard)],
            [*(hex_bytes(le(1, 8) + root + binary(msg["record"])) for root, msg in zip(roots, packets, strict=True)),
             *retained_data, "0x"],
            [*(wallet_group(i + lane_count, [key]) for i, key in enumerate(owners)),
             wallet_group(2 * lane_count, [fixture.faucet])])

    rejections["wrong_second_burn"] = reject_ckb(burn_both("wrong-burn", [*inbound[:-1], wrong]), 51)
    if partial:
        unchanged = [[lanes[0]["assets"][0]["units"], remaining[0][1]]]
        rejections["unchanged_component"] = reject_ckb(burn_both("unchanged-component", inbound, unchanged), 48, 51)
    assert all(l1.call("get_live_cell", location, False)["status"] == "live" for location in [*mailboxes, *wrapped])
    burned = submit_ckb(burn_both("burn", inbound))
    burn_points = [point(burned["hash"], i) for i in range(lane_count)]
    assert all(l1.call("get_live_cell", location, False)["status"] != "live" for location in wrapped)
    for i in range(lane_count):
        cell_info = l1.call("get_live_cell", point(burned["hash"], i + lane_count), True)["cell"]
        expected = lanes[i]["wrapper"] if partial else None
        assert cell_info["output"]["type"] == expected
        assert cell_info["data"]["content"] == (representation(lanes[i], remaining[i]) if partial else "0x")

    def attest(command, label, parameters):
        votes = [json.loads(run(args.binary, "bridge", command, "--genesis", spec_path, "--l1-rpc", l1.url,
            "--key-file", key, "--journal", args.output / f"metatype-batch-{command}-{i}", *parameters))
            for i, key in enumerate(fixture.guardian_keys[:3])]
        path = args.output / f"metatype-batch-{label}-approvals.json"
        write_json(path, votes)
        return path

    l1.mine(spec["bridge"]["confirmations"])
    _, previous = query("/stem/bridge-anchor")
    anchor = burned["proof"]["header"]
    height, hash_ = int(anchor["number"], 16), anchor["hash"]
    anchor_votes = attest("attest-anchor", "anchor", ["--previous-height", str(previous["height"]),
        "--previous-hash", previous["hash"], "--height", str(height)])
    advance = commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", anchor_votes)))

    def observations(label, points):
        request = args.output / f"metatype-batch-{label}-reference-request.json"
        write_json(request, l1_reference_request(height, hash_, *((location, False) for location in points)))
        return attest("attest-reference", label, ["--request", request])

    both = observations("both", burn_points)
    one = observations("first-only", burn_points[:-1]) if lane_count > 1 else None
    return_inputs = [*((location, lane["escrow"]) for location, lane in zip(held, lanes, strict=True)),
                     (stem_point(exported, lane_count), owner)]
    release = [*(cell(bond, lane["escrow"], amount_data(lane, remaining[i]), lane["types"])
                 if partial else cell(bond, lane["escrow"]) for i, lane in enumerate(lanes)),
               *(cell(release_capacity, lane["returning"], amount_data(lane, returning[i]), lane["types"])
                 for i, lane in enumerate(lanes)),
               cell(export_change - lane_count * release_capacity - FEE, owner)]
    receives = [(lane["escrow"], 0xff01, msg) for lane, msg in zip(lanes, inbound, strict=True)]
    rejections["missing_second_burn_reference"] = reject_stem(stem_signed("missing-second-burn", return_inputs,
        release, messages=receives, observations=one), "uniquely matching certified CKB mailbox output")
    redirected = release.copy()
    redirected[lane_count] = cell(release_capacity, owner, amount_data(lanes[0], returning[0]), lanes[0]["types"])
    rejections["misdirected_native_release"] = reject_stem(stem_signed("redirect-native", return_inputs,
        redirected, messages=receives, observations=both), "exited with 54")
    before = [certified("/stem/intercell/inbox", msg["id"]) for msg in inbound]
    unspent = list(map(cell_proof, held))
    assert all(item["record"] is None for item in before)
    assert all(item["cell"] for item in unspent)
    redeemed = commit(stem_signed("redeem", return_inputs, release, messages=receives, observations=both))
    replay_inputs = [(stem_point(redeemed, i), lane["escrow"]) for i, lane in enumerate(lanes)]
    replay_inputs.append((stem_point(redeemed, 2 * lane_count), owner))
    rejections["replayed_native_return"] = reject_stem(stem_signed("replay", replay_inputs,
        [*release[:lane_count], cell(export_change - lane_count * release_capacity - 2 * FEE, owner)],
        messages=receives, observations=both), "nonce already used")
    released = [stem_point(redeemed, i) for i in range(lane_count, 2 * lane_count)]
    spent = list(map(cell_proof, held))
    assert all(item["cell"] is None for item in spent)
    for i, location in enumerate(released):
        assert signing_cell(client, location)["cell"] == release[i + lane_count]
        assert signing_cell(client, stem_point(redeemed, i))["cell"] == release[i]
    finalized = [certified("/stem/intercell/inbox", msg["id"]) for msg in inbound]
    assert [item["record"] for item in finalized] == [msg["record"] for msg in inbound]
    released_proofs = list(map(cell_proof, released))
    retained_proofs = [cell_proof(stem_point(redeemed, i)) for i in range(lane_count)]
    if composed:
        lane = lanes[0]
        pieces = [cell(release_capacity // 2 - (FEE if i == 0 else 0), friend,
                       le(returning[0][i], 16), asset["types"]) for i, asset in enumerate(lane["assets"])]
        divided = commit(stem_signed("divide", [(released[0], friend)], pieces, signers=[(friend, fixture.friend)]))
        piece_points = [stem_point(divided, i) for i in range(len(pieces))]
        assert [signing_cell(client, item)["cell"] for item in piece_points] == pieces
        bundle.update(divided=execution(divided), pieces=list(map(cell_proof, piece_points)),
                      released_spent=cell_proof(released[0]))

    limit_keys = [fixture.owner, fixture.friend, *fixture.l1_keys]
    limit_locks = [script(native_lock_code, fixture.key(key)["blake160"], "data") for key in limit_keys]
    tiny_cell = 60 * CKB
    probe_funding = commit(stem_signed("limit-funding", [(stem_point(redeemed, 2 * lane_count), owner)],
        [*(cell(tiny_cell, lock) for lock in limit_locks),
         cell(export_change - lane_count * release_capacity - len(limit_locks) * tiny_cell - 2 * FEE, owner)]))
    probe_inputs = [(stem_point(probe_funding, i), lock) for i, lock in enumerate(limit_locks)]
    probes = [packet(f"limit-{i}", {**lanes[0], "escrow": lock}, True, 1, fixture.owner_lock)
              for i, lock in enumerate(limit_locks)]
    probe_actions = [(lock, 0xff00, msg) for lock, msg in zip(limit_locks, probes, strict=True)]

    def probe_tx(label, count):
        return stem_signed(label, probe_inputs[:count], [cell(count * tiny_cell - FEE, owner)],
                           messages=probe_actions[:count], signers=list(zip(limit_locks[:count], limit_keys[:count])))

    rejections["fifth_action"] = reject_stem(probe_tx("five-actions", 5), "Intercell action count exceeds transaction limit")
    absent = [certified("/stem/intercell/outbox", msg["id"]) for msg in probes]
    assert all(item["record"] is None for item in absent)
    four = commit(probe_tx("four-actions", 4))
    checked = [certified("/stem/intercell/outbox", msg["id"]) for msg in probes]
    assert [item["record"] for item in checked] == [msg["record"] for msg in probes[:4]] + [None]

    return {
        "shared_manifests": args.metatype_shared_manifest,
        "lanes": [{"units": ([asset["units"] for asset in lane["assets"]] if composed else lane["assets"][0]["units"]),
                   "returned_units": returning[i], "remaining_units": remaining[i],
                   "asset_ids": [hex_bytes(asset["id"]) for asset in lane["assets"]],
                   "types": lane["types"], "wrapper": lane["wrapper"],
                   "manifest": hex_bytes(lane["manifest"]), "mailbox": lane["mailbox"],
                   "occupied": occupied(lane["receiver"], lane["wrapper"], binary(representation(lane)))}
                  for i, lane in enumerate(lanes)],
        "metadata": l1.prove_committed(published), "created": l1.prove_committed(created),
        "issue": issue, "exported": exported, "minted": minted, "swapped": swapped, "burned": burned,
        "anchor": advance, "redeemed": redeemed, "outbound": outbound, "inbound": inbound,
        "outbox_before": outbound_before,
        "outbox": [certified("/stem/intercell/outbox", msg["id"]) for msg in outbound],
        "deliveries": deliveries,
        "inboxes_before": before, "inboxes": finalized, "escrows_before": unspent,
        "escrows_spent": spent, "escrows_retained": retained_proofs, "released": released_proofs,
        "rejections": rejections, "composition": bundle,
        "execution": {"export": execution(exported), "redeem": execution(redeemed)},
        "result_ids": [[msg["id"] for _, _, msg in sorted(batch, key=lambda row: packed_script(row[0]))]
                       for batch in (sends, receives)],
        "limit": {"committed": four, "execution": execution(four), "messages": probes,
                  "before": absent, "outbox": checked,
                  "result_ids": [msg["id"] for _, _, msg in
                                 sorted(probe_actions[:4], key=lambda row: packed_script(row[0]))]},
    }

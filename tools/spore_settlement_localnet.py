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
    to = le(0, 4) + packed_script(target) if target is not None else None
    tag = 3 if cluster else 0
    if target is None:
        detail = le(tag + 2, 4) + table(object_id, le(0, 4) + packed_script(source))
    elif source is None:
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
    native_origin = args.native_spore_origin is not None
    approved_origin = args.native_spore_approval is not None
    stem_spore_code = hex_bytes(ckb_hash((args.native_spore_origin or args.stem_spore).read_bytes()))
    native_lock_code = hex_bytes(ckb_hash(args.native_lock.read_bytes()))
    owner = script(native_lock_code, fixture.key(fixture.owner)["blake160"], "data")
    buyer = script(native_lock_code, fixture.key(fixture.friend)["blake160"], "data")
    mailbox_code, escrow_code = [ckb_hash(program) for program in (mailbox_bin, ckb_lock_bin)]
    secp = vault["secp"]
    spore_cap = ((650 if approved_origin else 480 if args.native_spore_multipart else 360) + args.spore_content_bytes) * CKB
    mailbox_cap, relay_funds = 1000 * CKB, 1800 * CKB
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
    if approved_origin:
        funds += [(150 * CKB, fixture.owner_lock), (150 * CKB, fixture.friend_lock)]
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

    sequencer_id = type_id(mint_source + 1)
    checkpoint_code, checkpoint_id = binary(vault["anchor"]["hash"]), vault["anchor"]["id"]
    if native_origin:
        reservation = script(hex_bytes(escrow_code), hex_bytes(sequencer_id + mailbox_code), "data2")
        reserve_tx = transaction([point(funded, mint_source)], [output(spore_cap, reservation)], ["0x"], [secp])
        reserve_hash = l1.send(fixture.sign("reserve-spore-id", reserve_tx, [wallet_group(0, [fixture.faucet])]))
        object_point = point(reserve_hash, 0)
        object_id = ckb_hash(le(0, 8) + binary(reserve_hash) + le(0, 4) + le(0, 8))
        object_type = script(hex_bytes(SPORE_CODE), hex_bytes(object_id), "data1")
        counterpart = script_hash(object_type)
        native_input = mint["payment"]
        native_id = ckb_hash(b"Stem object identity\0" + binary(network) + binary(native_input["tx_hash"])
                             + le(int(native_input["index"], 16), 4) + le(0, 4))
        authority = script_hash(fixture.standard)
        policy_code = ckb_hash(args.native_spore_approval.read_bytes()) if approved_origin else None
        native_args = native_id + (script_hash(owner) + authority + policy_code if approved_origin else b"") + counterpart
        mapped_type = script(stem_spore_code, hex_bytes(native_args), "data")
        original = script_hash(mapped_type)
        asset_types = [mapped_type]
        if approved_origin:
            asset_types.append(script(hex_bytes(policy_code), hex_bytes(original + script_hash(owner)), "data"))
        mime, content = (b"application/json", b'{"origin":"Stem"}') if args.native_spore_json else (b"text/plain", b"Issued on Stem")
        if args.spore_content_bytes:
            content = content.ljust(args.spore_content_bytes, b"x")
        if args.native_spore_multipart:
            mime = b"multipart/mixed;boundary=stem-boundary"
            content = b"--stem-boundary\r\nContent-Type: text/plain\r\n\r\nStem\r\n--stem-boundary--\r\n"
        data = table(molecule_bytes(mime + (b";immortal=true" if args.spore_immortal else b"")), molecule_bytes(content), b"")
        endpoint = script(stem_spore_code, hex_bytes((b"\x02" if approved_origin else b"\x01")
            + original + binary(genesis) + mailbox_code + sequencer_id + escrow_code + checkpoint_code
            + checkpoint_id + counterpart + (authority if approved_origin else b"")), "data")
        endpoint_id = script_hash(endpoint)
        ckb_escrow = script(hex_bytes(escrow_code),
            hex_bytes(sequencer_id + endpoint_id + original + mailbox_code + counterpart
                      + (authority if approved_origin else b"")), "data2")
    else:
        object_id = type_id(mint_source)
        object_type = script(hex_bytes(SPORE_CODE), hex_bytes(object_id), "data1")
        original = script_hash(object_type)
        def spore_data(id_):
            mime = b"text/plain;immortal=true" if args.spore_immortal else b"text/plain"
            content = b"One Spore across two networks".ljust(args.spore_content_bytes, b"x")
            return table(molecule_bytes(mime), molecule_bytes(content),
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
        if args.spore_immortal:
            melt = transaction([object_point], [output(spore_cap - FEE, fixture.owner_lock)], ["0x"], deps)
            melt["witnesses"] = ["0x", cobuild(object_type, object_id, fixture.owner_lock, None)]
            rejected["ckb_immortal_melt"] = ckb_reject(
                fixture.sign("spore-immortal-melt", melt, [wallet_group(0, [fixture.owner])]), 65)
            assert SPORE_CODE.hex() in rejected["ckb_immortal_melt"]
        endpoint = script(stem_spore_code, hex_bytes(original + binary(genesis) + mailbox_code
            + sequencer_id + escrow_code + checkpoint_code + checkpoint_id), "data")
        endpoint_id = script_hash(endpoint)
        ckb_escrow = script(hex_bytes(escrow_code),
            hex_bytes(sequencer_id + endpoint_id + original + mailbox_code), "data2")
        mapped_type = script(stem_spore_code, hex_bytes(endpoint_id), "data")
    mailbox = script(hex_bytes(mailbox_code), hex_bytes(sequencer_id + binary(genesis) + binary(network)
        + script_hash(ckb_escrow) + checkpoint_code + checkpoint_id), "data2")
    mailbox_id = script_hash(mailbox)
    initial = transaction([point(funded, mint_source + 1)], [output(mailbox_cap, ckb_escrow, mailbox)],
                          [hex_bytes(le(0, 8) + empty)], deps)
    initial["header_deps"] = [genesis]
    created = l1.send(fixture.sign("spore-mailbox", initial, [wallet_group(0, [fixture.faucet])]))
    mailbox_point = point(created, 0)
    l1_fee_point = point(funded, mint_source + 2)
    fee_capacity = relay_funds
    nonce_frontier = 0

    def packet(label, outbound, recipient, nonce, content=data, wire_content=None):
        source, destination, sender, receiver = ((network, genesis, endpoint_id, mailbox_id) if outbound
                                                 else (genesis, network, mailbox_id, endpoint_id))
        path = args.output / f"spore-{label}-message.json"
        if wire_content is None:
            wire_content = content if len(content) <= 4032 else b"SPH1" + le(len(content), 4) + ckb_hash(content)
        write_json(path, dict(source=source, destination=destination, sender=hex_bytes(sender),
            receiver=hex_bytes(receiver), nonce=nonce, payload=hex_bytes(original + recipient + wire_content)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", path))

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

    code_points = [mint["lock_dep"], point(mint["mint_id"], 2)]
    if approved_origin:
        code_points.append(point(mint["mint_id"], 3))

    def stem_signed(label, spending, outputs, *, actor=owner, key=fixture.owner, bond=None,
                    message=None, receive=False, observations=None, extra_points=(), cosigner=None):
        points = ([spending] if bond is None else [spending, bond]) + list(extra_points)
        locks = sorted([actor] + ([endpoint] if bond else []) + ([cosigner[0]] if cosigner else []), key=packed_script)
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
        for signer, secret in [(actor, key)] + ([cosigner] if cosigner else []):
            write_json(lock, signer)
            tx = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path, "--transaction", draft,
                              "--context", context, "--group-index", str(locks.index(signer)), "--lock", lock,
                              "--key-file", secret))
            draft = args.output / f"spore-{label}-stem-signed.json"
            write_json(draft, tx)
        return tx

    def at(tx, index):
        return point("0x" + tx["hash"], index)


    def delivery(label, message, consumed, root):
        height, outbox = query("/stem/intercell/outbox", message["id"])
        checkpoint = relay.advance(height, secp, probe=True)
        header, request = [args.output / f"spore-{label}-{name}.json" for name in ("header", "request")]
        write_json(header, hex_bytes(relay.header(height)))
        write_json(request, {**outbox, "consumed": consumed, "expected_root": hex_bytes(root)})
        encoded = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
        write_json(args.output / f"spore-{label}-witness.json", encoded)
        return checkpoint, encoded

    def export_native():
        bond_cap = 120 * CKB
        object_cap = ((800 if approved_origin else 360 if args.native_spore_multipart else 240)
                      + (args.spore_content_bytes+99)//100) * CKB
        remaining = mint["payment_capacity"] - bond_cap - object_cap - FEE
        empty_root = bytes(32)
        rejected = {}
        native_cell = cell(object_cap, owner, data, asset_types)
        issued_outputs = [native_cell, cell(bond_cap, endpoint), cell(remaining, owner)]
        if approved_origin:
            rejected["missing_native_policy"] = stem_reject(stem_signed("missing-policy", mint["payment"],
                [cell(object_cap, owner, data, [mapped_type]), *issued_outputs[1:]]), "exited with 117")
        else:
            fake = dict(mapped_type, args="0x" + "aa" * 32 + script_hash(object_type).hex())
            rejected["wrong_native_id"] = stem_reject(stem_signed("wrong-native-id", mint["payment"],
                [cell(object_cap, owner, data, [fake]), *issued_outputs[1:]]), "exited with 117")
            additional = dict(mapped_type, args=mapped_type["args"][:66] + "bb" * 32)
            rejected["attached_second_type"] = stem_reject(stem_signed("composite-native", mint["payment"],
                [cell(object_cap, owner, data, [mapped_type, additional]), *issued_outputs[1:]]), "exited with 117")
        if args.native_spore_multipart:
            broken = data.replace(b"\r\n--stem-boundary--", b"\r\n--stem-xxxxxxxx--")
            assert len(broken) == len(data) and broken != data
            rejected["invalid_native_boundary"] = stem_reject(stem_signed("bad-boundary", mint["payment"],
                [cell(object_cap, owner, broken, [mapped_type]), *issued_outputs[1:]]), "exited with 117")
        issued = commit(stem_signed("issue-native", mint["payment"], issued_outputs))
        original_point, bond_point, fee_point = [at(issued, i) for i in range(3)]

        native_transfers = []
        if approved_origin:
            gift = commit(stem_signed("gift", fee_point, [cell(object_cap, buyer, data, asset_types),
                cell(remaining-FEE, owner)], extra_points=[original_point]))
            rejected["native_without_approval"] = stem_reject(stem_signed("buyer-only", at(gift, 0),
                [cell(object_cap-FEE, owner, data, asset_types)], actor=buyer, key=fixture.friend), "exited with 115")
            remaining -= 2*FEE
            returned = commit(stem_signed("buyer-with-approval", at(gift, 1),
                [native_cell, cell(remaining, owner)], extra_points=[at(gift, 0)], cosigner=(buyer, fixture.friend)))
            original_point, fee_point = at(returned, 0), at(returned, 1)
            native_transfers = [gift, returned]

        def holder(wallet, *, gate=None, approver=None):
            if not approved_origin:
                return wallet
            fields = b"\x01" + counterpart + (approver or authority) + (gate or script_hash(ckb_escrow)) + script_hash(wallet)
            return script(hex_bytes(escrow_code), hex_bytes(fields), "data2")

        owner_holder, friend_holder = holder(fixture.owner_lock), holder(fixture.friend_lock)
        def outbound(label, bond_location, wire_content=None):
            key = binary(bond_location["tx_hash"]) + le(int(bond_location["index"], 16), 4)
            nonce = int.from_bytes(ckb_hash(b"Stem Spore return nonce\0" + key)[:8], "little")
            return packet(label, True, script_hash(fixture.owner_lock), nonce, wire_content=wire_content)

        def lock_asset(label, location, bond_location, fee_location, capacity, msg):
            return stem_signed(label, fee_location, [cell(bond_cap, endpoint),
                cell(object_cap, endpoint, data, asset_types), cell(capacity - FEE, owner)],
                bond=bond_location, extra_points=[location], message=msg)

        if args.spore_immortal:
            rejected["native_immortal_melt"] = stem_reject(stem_signed("native-immortal-melt", original_point,
                [cell(object_cap - FEE, owner)]), "exited with 117")

        send = outbound("initial-export", bond_point)
        if args.spore_content_bytes:
            for label, length, digest in [("wrong_content_hash", len(data), bytes(32)),
                                          ("wrong_content_length", len(data)-1, ckb_hash(data))]:
                changed = outbound(label, bond_point, b"SPH1"+le(length, 4)+digest)
                rejected[label] = stem_reject(lock_asset(label, original_point, bond_point, fee_point,
                                                         remaining, changed), "exited with 119")
        rejected["changed_native_content"] = stem_reject(stem_signed("edited-native", original_point,
            [cell(object_cap - FEE, owner, data[:-1] + b"?", asset_types)]), "exited with 117")
        locked = commit(lock_asset("lock-origin", original_point, bond_point, fee_point, remaining, send))
        custody = cell_proof(at(locked, 1))
        assert custody["cell"] and custody["proof"]
        checkpoint, encoded = delivery("initial-export", send, [], empty_root)

        def mint_standard(label, *, recipient=owner_holder, content=data,
                          use_mailbox=True, include_checkpoint=True, valid_witness=True, no_approval=False):
            inputs = [object_point, mailbox_point, l1_fee_point] if use_mailbox else [object_point, l1_fee_point]
            change, signer = output(fee_capacity-FEE, fixture.standard), fixture.faucet
            if no_approval:
                inputs[-1] = point(funded, mint_source+3)
                change, signer = output(150*CKB-FEE, fixture.owner_lock), fixture.owner
            outputs = [output(spore_cap, recipient, object_type)]
            values = [hex_bytes(content)]
            witnesses = ["0x"]
            if use_mailbox:
                outputs.append(output(mailbox_cap, ckb_escrow, mailbox))
                values.append(hex_bytes(le(0, 8) + binary(encoded["successor_root"])))
                witnesses.append(witness(binary(encoded["input_type"])))
            tx = transaction(inputs, outputs + [change], values + ["0x"],
                             deps + ([checkpoint] if include_checkpoint else []))
            tx["witnesses"] = witnesses + ["0x"]
            if valid_witness:
                tx["witnesses"].append(cobuild(object_type, object_id, None, recipient, data=content))
            return fixture.sign(label, tx, [wallet_group(len(inputs)-1, [signer])])

        rejected["seed_without_mailbox"] = ckb_reject(mint_standard("seed-only", use_mailbox=False), 114)
        rejected["mint_without_checkpoint"] = ckb_reject(mint_standard("no-checkpoint", include_checkpoint=False), 84)
        rejected["wrong_mint_owner"] = ckb_reject(mint_standard("different-owner", recipient=friend_holder), 113)
        changed_content = data.replace(b"Stem", b"STEM", 1)
        assert changed_content != data
        rejected["changed_export_data"] = ckb_reject(mint_standard("changed-data", content=changed_content), 113)
        rejected["missing_standard_mint_action"] = ckb_reject(mint_standard("no-cobuild", valid_witness=False))
        assert SPORE_CODE.hex() in rejected["missing_standard_mint_action"]
        if approved_origin:
            rejected["mint_to_plain_owner"] = ckb_reject(mint_standard("no-policy-holder", recipient=fixture.owner_lock), 115)
            rejected["mint_without_approval"] = ckb_reject(mint_standard("no-mint-approval", no_approval=True), 115)
        standard = submit_ckb(mint_standard("standard-spore"))
        spore, sequencer, fees = [point(standard["hash"], i) for i in range(3)]
        actual = l1.call("get_live_cell", spore, True)["cell"]
        assert actual["output"] == output(spore_cap, owner_holder, object_type)
        assert actual["data"]["content"] == hex_bytes(data)
        if args.spore_immortal:
            melted = transaction([spore], [output(spore_cap - FEE, fixture.owner_lock)], ["0x"], deps[:2])
            melted["witnesses"] = ["0x", cobuild(object_type, object_id, fixture.owner_lock, None)]
            rejected["standard_immortal_melt"] = ckb_reject(fixture.sign("standard-immortal-melt", melted,
                [wallet_group(0, [fixture.owner])]), 65)
            assert SPORE_CODE.hex() in rejected["standard_immortal_melt"]
        if approved_origin:
            def approved_move(label, *, with_owner=True, with_approval=True, next_holder=friend_holder, melt=False):
                inputs, signers = [spore], []
                outputs = [output(spore_cap, next_holder, object_type)] if not melt else [output(spore_cap, fixture.owner_lock)]
                values = [hex_bytes(data) if not melt else "0x"]
                if with_approval:
                    signers.append(wallet_group(len(inputs), [fixture.faucet]))
                    inputs.append(fees)
                    outputs.append(output(fee_capacity-2*FEE, fixture.standard))
                    values.append("0x")
                if with_owner:
                    signers.append(wallet_group(len(inputs), [fixture.owner]))
                    inputs.append(point(funded, mint_source+3))
                    outputs.append(output(150*CKB - (0 if with_approval else FEE), fixture.owner_lock))
                    values.append("0x")
                move = transaction(inputs, outputs, values, [*deps[:2], deps[3]])
                move["witnesses"] = ["0x"]*len(inputs) + [cobuild(object_type, object_id, owner_holder,
                    None if melt else next_holder)]
                return fixture.sign(label, move, signers)
            for label, options in [
                ("missing_owner", {"with_owner": False}), ("missing_approval", {"with_approval": False}),
                ("stripped_policy", {"next_holder": fixture.friend_lock}), ("policy_melt", {"melt": True}),
                ("replaced_authority", {"next_holder": holder(fixture.friend_lock, approver=script_hash(fixture.friend_lock))}),
                ("replaced_gate", {"next_holder": holder(fixture.friend_lock, gate=script_hash(fixture.owner_lock))}),
            ]:
                rejected[label] = ckb_reject(approved_move(label, **options), 115)
            moved = submit_ckb(approved_move("ordinary-wallet-approved"))
        else:
            # An ordinary CKB wallet can transfer the asset using only Spore and its owner lock.
            moved = transaction([spore, fees], [output(spore_cap, fixture.friend_lock, object_type),
                output(fee_capacity-2*FEE, fixture.standard)], [hex_bytes(data), "0x"], deps[:2])
            moved["witnesses"] = ["0x", "0x", cobuild(object_type, object_id, fixture.owner_lock, fixture.friend_lock)]
            moved = submit_ckb(fixture.sign("ckb-wallet-transfer", moved,
                [wallet_group(0, [fixture.owner]), wallet_group(1, [fixture.faucet])]))
        spore, fees = [point(moved["hash"], i) for i in range(2)]

        inbound = packet("return-native", False, script_hash(owner), 0)
        deposit_inputs = [sequencer, spore, fees]
        deposit_outputs = [output(mailbox_cap, ckb_escrow, mailbox), output(spore_cap, ckb_escrow, object_type),
                           output(fee_capacity-3*FEE, fixture.standard)]
        deposit_data = [hex_bytes(le(1, 8) + binary(encoded["successor_root"]) + binary(inbound["record"])),
                        hex_bytes(data), "0x"]
        deposit_signers = [wallet_group(2, [fixture.faucet])]
        if approved_origin:
            deposit_inputs.append(point(funded, mint_source+4))
            deposit_outputs.append(output(150*CKB, fixture.friend_lock))
            deposit_data.append("0x")
            deposit_signers.append(wallet_group(3, [fixture.friend]))
        else:
            deposit_signers.insert(0, wallet_group(1, [fixture.friend]))
        return_tx = transaction(deposit_inputs, deposit_outputs, deposit_data, deps)
        return_tx["witnesses"] = ["0x"]*len(deposit_inputs) + [cobuild(object_type, object_id, friend_holder, ckb_escrow)]
        held = submit_ckb(fixture.sign("ckb-return", return_tx, deposit_signers))
        anchor, approved = observe_deposit("return-native", held)
        restored_outputs = [cell(bond_cap, endpoint), native_cell, cell(remaining-2*FEE, owner)]
        restored = commit(stem_signed("restore-origin", at(locked, 2), restored_outputs,
            bond=at(locked, 0), extra_points=[at(locked, 1)], message=inbound, receive=True, observations=approved))
        restored_cell = cell_proof(at(restored, 1))
        assert signing_cell(client, at(restored, 1))["cell"] == native_cell

        second = outbound("repeat-export", at(restored, 0))
        relocked = commit(lock_asset("repeat-lock", at(restored, 1), at(restored, 0),
                                      at(restored, 2), remaining-2*FEE, second))
        next_checkpoint, next_encoded = delivery("repeat-export", second, [send["id"]], binary(encoded["successor_root"]))
        sequencer, spore, fees = [point(held["hash"], i) for i in range(3)]
        release = transaction([sequencer, spore, fees],
            [output(mailbox_cap, ckb_escrow, mailbox), output(spore_cap, owner_holder, object_type),
             output(fee_capacity-4*FEE, fixture.standard)],
            [hex_bytes(le(1, 8) + binary(next_encoded["successor_root"])), hex_bytes(data), "0x"],
            deps + [next_checkpoint])
        release["witnesses"] = [witness(binary(next_encoded["input_type"])), "0x", "0x",
                               cobuild(object_type, object_id, ckb_escrow, owner_holder)]
        released = submit_ckb(fixture.sign("release-existing", release, [wallet_group(2, [fixture.faucet])]))
        final = l1.call("get_live_cell", point(released["hash"], 1), True)["cell"]
        assert final["output"] == actual["output"] and final["data"] == actual["data"]
        melt_result = None
        if not args.spore_immortal and not approved_origin:
            last = point(released["hash"], 1)
            melt = transaction([last], [output(spore_cap - FEE, fixture.owner_lock)], ["0x"], deps[:2])
            melt["witnesses"] = ["0x", cobuild(object_type, object_id, fixture.owner_lock, None)]
            melted = submit_ckb(fixture.sign("standard-melt", melt, [wallet_group(0, [fixture.owner])]))
            assert l1.call("get_live_cell", last, True)["status"] != "live"
            held_native = cell_proof(at(relocked, 1))
            assert signing_cell(client, at(relocked, 1))["cell"] == cell(object_cap, endpoint, data, asset_types)
            melt_result = {"transaction": melted, "native_custody": held_native}
        return {"origin": "stem", "immortal": args.spore_immortal, "spore_id": hex_bytes(object_id), "spore_type": object_type,
                "native_type": mapped_type, "native_content": hex_bytes(data), "mailbox": mailbox, "endpoint": endpoint,
                "reservation": object_point, "issued": issued, "locked": locked, "native_custody": custody,
                "minted": standard, "ckb_wallet_transfer": moved, "deposited": held, "return_anchor": anchor,
                "restored": restored, "restored_cell": restored_cell, "relocked": relocked,
                "released_same_spore": released, "original_ckb_cell": actual, "final_ckb_cell": final,
                "native_wallet_transfers": native_transfers, "native_types": asset_types,
                "standard_melt": melt_result, "rejections": rejected}

    if native_origin:
        return export_native()

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

    if args.spore_content_bytes:
        for label, length, digest in [("ckb_wrong_content_hash", len(data), bytes(32)),
                                      ("ckb_wrong_content_length", len(data)-1, ckb_hash(data))]:
            msg = packet(label, False, script_hash(owner), 0, wire_content=b"SPH1"+le(length, 4)+digest)
            rejected[label] = ckb_reject(deposit(label, policy=msg), 113)
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

    anchor, approved = observe_deposit("import", held)
    bond, mapped_cap = 100 * CKB, (80 + (args.spore_content_bytes+99)//100) * CKB
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

    checkpoint, encoded = delivery("outbox", returning, [], empty)
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

    return {"id": hex_bytes(object_id), "immortal": args.spore_immortal, "original_type": object_type, "mapped_type": mapped_type, "cluster": cluster,
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

"""Verify real Spore V2 custody, Stem ownership and CKB redemption."""

import hashlib
import json
import secrets
import subprocess

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, l1_reference_request, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group, write_key)
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell
from stake_localnet import witness

# Published Spore V2 mainnet code and the CoBuild ScriptInfo hash used by CCC.
SPORE_CODE = bytes.fromhex("4a4dce1df3dffff7f8b2cd7dff7303df3b6150c9788cb75dcf6747247132b9f5")
SPORE_BETA2 = bytes.fromhex("685a60219309029d01310311dba953d67029170ca4848a4ff638e57002130a0d")
MUTANT_BETA2 = bytes.fromhex("5ff1a403458b436ea4b2ceb72f1fa70a6507968493315b646f5302661cb68e57")
LUA_LIB = bytes.fromhex("ed08faee8c29b7a7c29bd9d495b4b93cc207bd70ca93f7b356f39c677e7ab0fc")
CLUSTER_CODE = bytes.fromhex("7366a61534fa7c7e6225ecc0d828ea3b5366adec2b58206f2ee84995fe030075")
CLUSTER_BETA2 = bytes.fromhex("0bbe768b519d8ea7b96d58f1182eb7e6ef96c541fbd9526975077ee09f049058")
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


def wallet_history(binary_path, directory, l1_port, name, request):
    request_path = directory / f"{name}-history-request.json"
    proof_path = directory / f"{name}-history-proof.json"
    write_json(request_path, request)
    with proof_path.open("wb") as destination:
        subprocess.run([binary_path, "ckb-wallet", "prove-type-history", "--request", request_path,
                        "--l1-rpc", f"http://127.0.0.1:{l1_port}"], stdout=destination, check=True)
    result = json.loads(run(binary_path, "ckb-wallet", "verify-type-history", "--request", request_path,
                            "--proof", proof_path))
    with proof_path.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    return {"request": request, "verified": result, "proof_sha256": digest, "proof_bytes": proof_path.stat().st_size}


def verify_spore_history(binary_path, directory, l1_port, report):
    native = report.get("origin") == "stem"
    custody_changes = report.get("handoffs", [])
    if report.get("stale_handoff"):
        custody_changes = [*custody_changes, report["stale_handoff"]]
    if native:
        object_type = report["spore_type"]
        events = [report[k] for k in ("minted", "ckb_wallet_transfer", "deposited")]
        events.extend(step["ckb"] for step in custody_changes)
        events.append(report["released_same_spore"])
        if report["standard_melt"]:
            events.append(report["standard_melt"]["transaction"])
    else:
        object_type = report["original_type"]
        events = [report["minted"], report["held"], *[step["ckb"] for step in custody_changes],
                  report["released"], report["redeposited"]]
    request = {
        "anchor": events[-1]["inclusion"]["proof"]["block_hash"],
        "type_hash": hex_bytes(script_hash(object_type)),
        "origin": point(events[0]["hash"], 0),
        "end_transaction": events[-1]["hash"],
    }
    if "mutant_registration" in report:
        if not native:
            events.insert(-1, report["ckb_wallet_transfer"])
        request["required_dependencies"] = report["mutant_registration"]["required_dependencies"]
        if report.get("cluster"):
            collection = report["cluster"]
            request["required_dependencies"] = [*request["required_dependencies"], {"data_hash": hex_bytes(CLUSTER_BETA2)},
                {"type_hash": hex_bytes(script_hash(collection["type"])), "data_hash": collection["data_hash"]}]
    deposit_name = "ckb-return" if native else "spore-deposit"
    deposit = json.loads((directory / f"{deposit_name}.json").read_text())
    index, = [i for i, cell in enumerate(deposit["outputs"]) if cell["type"] == object_type]
    request["required_states"] = [{
        "lock_hash": hex_bytes(script_hash(deposit["outputs"][index]["lock"])),
        "data_hash": hex_bytes(ckb_hash(binary(deposit["outputs_data"][index]))),
    }]
    for grant in custody_changes:
        request["required_states"].append({"lock_hash": hex_bytes(script_hash(grant["next_custody"])),
                                           "data_hash": request["required_states"][0]["data_hash"]})
    history = wallet_history(binary_path, directory, l1_port, "spore", request)
    result = history["verified"]
    assert [r["transaction"] for r in result["transitions"]] == [e["hash"] for e in events]
    assert result["matched_states"] == [point(report["deposited" if native else "held"]["hash"], index),
                                       *[point(step["ckb"]["hash"], 1) for step in custody_changes]]
    assert result["ended_in_burn"] == bool(report.get("standard_melt"))
    if "mutant_registration" in report and report.get("cluster"):
        cluster = report["cluster"]
        custody = [cluster["minted"]]
        if cluster.get("transferred"):
            custody.append(cluster["transferred"])
        collection_request = {"anchor": request["anchor"], "type_hash": hex_bytes(script_hash(cluster["type"])),
                              "origin": point(custody[0]["hash"], 0), "end_transaction": custody[-1]["hash"],
                              "required_dependencies": [{"data_hash": hex_bytes(CLUSTER_BETA2)}]}
        collection = wallet_history(binary_path, directory, l1_port, "collection", collection_request)
        states = collection["verified"]["transitions"]
        assert [s["transaction"] for s in states] == [t["hash"] for t in custody]
        assert all(s["output"]["data_hash"] == cluster["data_hash"] for s in states)
        history["collection"] = collection
    return history


def spore_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    genesis = vault["genesis"]
    network = client.network
    empty = bytes(32)
    code = args.spore_v2.read_bytes()
    mutant = args.spore_mutant_lua is not None
    spore_hash = SPORE_BETA2 if mutant else SPORE_CODE
    assert ckb_hash(code) == spore_hash, "the fixture requires the published Spore V2 ELF"
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
    spore_cap = ((650 if approved_origin else 480 if args.native_spore_multipart or mutant else 360) + args.spore_content_bytes) * CKB
    mailbox_cap, relay_funds = 1000 * CKB, 1800 * CKB
    codes = [code, mailbox_bin, ckb_lock_bin]
    fund_input, available = vault["change_point"], vault["change"]
    cluster_code = args.spore_cluster_v2.read_bytes() if args.spore_cluster_v2 else None
    cluster_cap, proxy_cap = 300 * CKB, 120 * CKB if cluster_code else 0
    if cluster_code:
        cluster_hash = CLUSTER_BETA2 if mutant else CLUSTER_CODE
        assert ckb_hash(cluster_code) == cluster_hash, "the fixture requires the release-matched Cluster ELF"
        if mutant:
            # The combined published programs exceed CKB's single-transaction size limit.
            capacity = (len(cluster_code) + 100) * CKB
            available -= capacity + FEE
            publish = transaction([fund_input], [output(capacity, fixture.standard), output(available, fixture.standard)],
                                  [hex_bytes(cluster_code), "0x"], [secp])
            deployment = l1.send(fixture.sign("publish-cluster", publish, [wallet_group(0, [fixture.faucet])]))
            cluster_dep, fund_input = dependency(point(deployment, 0)), point(deployment, 1)
        else:
            codes.append(cluster_code)
    if mutant:
        lua_code, lua_lib = args.spore_mutant_lua.read_bytes(), args.ckb_lua_library.read_bytes()
        assert ckb_hash(lua_code) == MUTANT_BETA2 and ckb_hash(lua_lib) == LUA_LIB
        lua_index = len(codes)
        codes.extend([lua_code, lua_lib])
        programs = [path.read_bytes() for path in args.spore_mutant_program or ()] or [b"print('hello world')"]
        spore_cap += (len(programs) - 1) * 65 * CKB
    issuer = None
    if args.ckb_spore_permit:
        issuer_code = args.ckb_spore_permit.read_bytes()
        issuer_dep_index = len(codes)
        codes.append(issuer_code)
        signers = sorted(map(script_hash, (fixture.standard, fixture.owner_lock)))
        issuer = script(hex_bytes(ckb_hash(issuer_code)), hex_bytes(b"".join(signers)), "data2")
    capacities = [(len(binary_) + 100) * CKB for binary_ in codes]
    funds = [(spore_cap + (0 if native_origin else proxy_cap) + FEE, fixture.standard), (mailbox_cap + FEE, fixture.standard),
             (relay_funds, fixture.standard)]
    if cluster_code:
        funds.append((cluster_cap + FEE, fixture.standard))
        if not native_origin and not mutant:
            funds.append((spore_cap + proxy_cap + FEE, fixture.owner_lock))
    if mutant:
        registration_index = len(codes) + len(funds)
        funds.extend(((200 + len(program)) * CKB + FEE, fixture.standard) for program in programs)
    if args.spore_handoff:
        holder_fund_index = len(codes) + len(funds)
        funds.extend((150 * CKB, lock) for lock in (fixture.owner_lock, fixture.friend_lock))
    if approved_origin:
        funds += [(150 * CKB, fixture.owner_lock), (150 * CKB, fixture.friend_lock)]
    if issuer:
        permit_index = len(codes) + len(funds)
        funds += [(1000 * CKB, fixture.standard), (150 * CKB, fixture.owner_lock),
                  (600 * CKB, fixture.standard), (150 * CKB, fixture.friend_lock)]
    payments = []
    if args.spore_mutant_separate_payee or args.spore_mutant_payee_per_program:
        payment_total = args.spore_mutant_payment * len(programs)
        # Preload the price onto the faucet input the mint already spends.
        # A native export uses the relay fee Cell. A CKB mint uses its issuance input.
        payer_index = 2 if native_origin else 0
        amount, lock = funds[payer_index]
        funds[payer_index] = (amount + payment_total, lock)
        prices = [args.spore_mutant_payment] * len(programs) if args.spore_mutant_payee_per_program else [payment_total]
        for i, price in enumerate(prices):
            name = f"registration-payee-{i}" if args.spore_mutant_payee_per_program else "registration-payee"
            key = write_key(args.output, name, secrets.token_bytes(32))
            payee = {**fixture.standard, "args": fixture.key(key)["blake160"]}
            payments.append((len(codes) + len(funds), payee, key, price))
            funds.append((100 * CKB, payee))
    change = available - sum(capacities) - sum(amount for amount, _ in funds) - FEE
    if payments and change < 61 * CKB:
        raise ValueError("insufficient fixture capacity for the payments and standard change Cell")
    fund = transaction([fund_input],
        [output(amount, fixture.standard) for amount in capacities] +
        [output(amount, lock) for amount, lock in funds] +
        [output(change, fixture.standard)],
        [hex_bytes(binary_) for binary_ in codes] + ["0x"] * (len(funds) + 1), [secp])
    funded = l1.send(fixture.sign("spore-fund", fund, [wallet_group(0, [fixture.faucet])]))
    deps = [secp] + [dependency(point(funded, i)) for i in range(3)]

    def sign_mint(label, tx, group):
        groups = [group]
        for payment_index, payee, payee_key, price in payments:
            payee_input = len(tx["inputs"])
            tx["inputs"].append({"previous_output": point(funded, payment_index), "since": "0x0"})
            tx["outputs"].append(output(100 * CKB + price, payee))
            tx["outputs_data"].append("0x")
            tx["witnesses"].insert(payee_input, "0x")
            groups.append(wallet_group(payee_input, [payee_key]))
        return fixture.sign(label, tx, groups)

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

    mutant_registration = None
    if mutant:
        deps += [dependency(point(funded, i)) for i in (lua_index, lua_index + 1)]
        entries = []
        required = [{"data_hash": hex_bytes(h)} for h in (spore_hash, MUTANT_BETA2, LUA_LIB)]
        payment = b"" if args.spore_mutant_payment is None else le(args.spore_mutant_payment, 8)
        mutant_ids = []
        for offset, program in enumerate(programs):
            owner_index = offset if args.spore_mutant_payee_per_program else 0
            registration_owner = payments[owner_index][1] if payments else fixture.owner_lock
            index = registration_index + offset
            mutant_id = type_id(index)
            mutant_ids.append(mutant_id.hex().encode())
            registry = script(hex_bytes(MUTANT_BETA2), hex_bytes(mutant_id + payment), "data1")
            capacity = funds[index - len(codes)][0] - FEE
            create = transaction([point(funded, index)], [output(capacity, registration_owner, registry)],
                                 [hex_bytes(program)], deps)
            name = "register-mutant" if offset == 0 else f"register-mutant-{offset + 1}"
            registered = submit_ckb(fixture.sign(name, create, [wallet_group(0, [fixture.faucet])]))
            entries.append({"type": registry, "program": program.decode(), "created": registered})
            required.append({"data_hash": hex_bytes(ckb_hash(program)), "type_hash": hex_bytes(script_hash(registry))})
        deps.extend(dependency(point(entry["created"]["hash"], 0)) for entry in entries)
        mutant_suffix = b";mutant[]=" + b",".join(mutant_ids)
        mutant_registration = {**(entries[0] if len(entries) == 1 else {"entries": entries}), "required_dependencies": required}
    mint_source = len(codes)
    cluster_id, cluster = b"", None
    rejected = {}
    if cluster_code:
        cluster_id = type_id(mint_source + 3)
        cluster_type = script(hex_bytes(cluster_hash), hex_bytes(cluster_id), "data1")
        cluster_data = table(molecule_bytes(b"Cross-chain collection"), molecule_bytes(b"CKB custody"), b"")
        if not mutant:
            cluster_dep = dependency(point(funded, 3))
        creation = transaction([point(funded, mint_source + 3)],
            [output(cluster_cap, fixture.standard, cluster_type)], [hex_bytes(cluster_data)],
            [secp, cluster_dep])
        creation["witnesses"] = ["0x", cobuild(cluster_type, cluster_id, None, fixture.standard,
                                                  data=cluster_data, cluster=True)]
        created_cluster = submit_ckb(fixture.sign("spore-cluster-mint", creation, [wallet_group(0, [fixture.faucet])]))
        cluster_point = point(created_cluster["hash"], 0)
        cluster = {"id": hex_bytes(cluster_id), "type": cluster_type, "data_hash": hex_bytes(ckb_hash(cluster_data)),
                   "minted": created_cluster}
        if mutant:
            deps.extend([cluster_dep, dependency(cluster_point)])

    sequencer_id = type_id(mint_source + 1)
    checkpoint_code, checkpoint_id = binary(vault["anchor"]["hash"]), vault["anchor"]["id"]
    if native_origin:
        reservation = script(hex_bytes(escrow_code), hex_bytes(sequencer_id + mailbox_code), "data2")
        reserve_tx = transaction([point(funded, mint_source)], [output(spore_cap, reservation)], ["0x"], [secp])
        reserve_hash = l1.send(fixture.sign("reserve-spore-id", reserve_tx, [wallet_group(0, [fixture.faucet])]))
        object_point = point(reserve_hash, 0)
        object_id = ckb_hash(le(0, 8) + binary(reserve_hash) + le(0, 4) + le(0, 8))
        object_type = script(hex_bytes(spore_hash), hex_bytes(object_id), "data1")
        counterpart = script_hash(object_type)
        native_input = mint["payment"]
        native_id = ckb_hash(b"Stem object identity\0" + binary(network) + binary(native_input["tx_hash"])
                             + le(int(native_input["index"], 16), 4) + le(0, 4))
        authority = script_hash(fixture.standard)
        policy_code = ckb_hash(args.native_spore_approval.read_bytes()) if approved_origin else None
        native_authority = script_hash(issuer) if issuer else script_hash(owner)
        native_args = native_id + (native_authority + authority + policy_code if approved_origin else b"") + counterpart
        route_fields = binary(genesis) + mailbox_code + sequencer_id + escrow_code + checkpoint_code + checkpoint_id
        if mutant:
            native_args = b"\x04" + native_id + counterpart + ckb_hash(route_fields)
        mapped_type = script(stem_spore_code, hex_bytes(native_args), "data")
        original = script_hash(mapped_type)
        asset_types = [mapped_type]
        if approved_origin:
            asset_types.append(script(hex_bytes(policy_code), hex_bytes(original + native_authority), "data"))
        mime, content = (b"application/json", b'{"origin":"Stem"}') if args.native_spore_json else (b"text/plain", b"Issued on Stem")
        if args.spore_content_bytes:
            content = content.ljust(args.spore_content_bytes, b"x")
        if args.native_spore_multipart:
            mime = b"multipart/mixed;boundary=stem-boundary"
            content = b"--stem-boundary\r\nContent-Type: text/plain\r\n\r\nStem\r\n--stem-boundary--\r\n"
        if mutant:
            mime += mutant_suffix
        data = table(molecule_bytes(mime + (b";immortal=true" if args.spore_immortal else b"")), molecule_bytes(content),
                     molecule_bytes(cluster_id) if cluster else b"")
        endpoint = script(stem_spore_code, hex_bytes((b"\x03" if mutant else b"\x02" if approved_origin else b"\x01")
            + original + route_fields + counterpart + (authority if approved_origin else b"")), "data")
        endpoint_id = script_hash(endpoint)
        ckb_escrow = script(hex_bytes(escrow_code),
            hex_bytes((b"\x03" if mutant else b"") + sequencer_id + endpoint_id + original + mailbox_code + counterpart
                      + (authority if approved_origin else b"")), "data2")
    else:
        object_id = type_id(mint_source)
        object_type = script(hex_bytes(spore_hash), hex_bytes(object_id), "data1")
        original = script_hash(object_type)
        def spore_data(id_):
            mime = b"text/plain;immortal=true" if args.spore_immortal else b"text/plain"
            if mutant:
                mime += mutant_suffix
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
                if include_cluster and not mutant:
                    dependencies.append(dependency(cluster_point))
            tx = transaction([point(funded, index)], outputs, payload, dependencies)
            tx["witnesses"] = ["0x", cobuild(type_, id_, None, fixture.owner_lock, data=data)]
            return sign_mint(f"spore-{label}", tx, wallet_group(0, [key]))

        if cluster and not mutant:
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
        assert result.get("check_tx", {}).get("code") == 0, result
        return result

    def stem_reject(tx, reason):
        result = client.call("broadcast_tx_sync", tx=tx)
        expected = (reason,) if isinstance(reason, str) else reason
        assert result["code"] == 1 and any(message in result["log"] for message in expected), result
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

    def reference_votes(label, header, *targets):
        request = args.output / f"spore-{label}-reference-request.json"
        write_json(request, l1_reference_request(int(header["number"], 16), header["hash"], *targets))
        return attest("attest-reference", f"{label}-reference", ["--request", request], journal="reference")

    def observe_deposit(label, deposit_, *targets):
        l1.mine(spec["bridge"]["confirmations"])
        _, prior_anchor = query("/stem/bridge-anchor")
        ckb_header = deposit_["inclusion"]["header"]
        height = int(ckb_header["number"], 16)
        approvals = attest("attest-anchor", f"{label}-anchor", ["--previous-height", str(prior_anchor["height"]),
            "--previous-hash", prior_anchor["hash"], "--height", str(height)], journal="anchor")
        anchor = commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", approvals)))
        return anchor, reference_votes(label, ckb_header, *(targets or [(point(deposit_["hash"], 0), False)]))

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
        build = subprocess.run([args.binary, "wallet", "build", "--request", req],
                               text=True, capture_output=True, check=True)
        write_json(draft, json.loads(build.stdout))
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

    def execution(committed):
        location = client.locations[committed["hash"]]
        height = location["height"]
        response = client.query("/stem/execution", height=height,
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return {**json.loads(bytes.fromhex(response["value"])), "height": height,
                "finality": client.finality(height)["proof"]}


    def custody_wallet(wallet):
        fields = b"\x05" + script_hash(ckb_escrow) + script_hash(object_type) + binary(native_lock_code) + binary(wallet["args"])
        return script(hex_bytes(escrow_code), hex_bytes(fields), "data2")

    def apply_handoffs(initial, last_ckb, nonce, consumed_root, object_cap, bond_cap, native_fees, ckb_fees):
        current = initial
        return_lock = custody_wallet(fixture.owner_lock)
        handoffs = []
        stale_handoff = None
        native_error = "exited with 119" if native_origin else "exited with 109"
        parties = [(owner, fixture.owner_lock, fixture.owner), (buyer, fixture.friend_lock, fixture.friend)]
        authorizers = [point(funded, holder_fund_index+i) for i in range(2)]
        for i in range(2 + int(args.spore_handoff_stale_source)):
            person = i % 2
            previous, wallet, secret = parties[person]
            following, next_wallet, _ = parties[1-person]
            label = f"handoff-{i+1}"
            source = at(current, 1)
            grant_source = at(initial, 1) if i == 2 else source
            next_cell = cell(object_cap, following, data, [mapped_type])
            req = args.output / f"{label}-request.json"
            write_json(req, {"object": hex_bytes(script_hash(mapped_type)), "original_type": hex_bytes(original),
                             "previous_owner": hex_bytes(script_hash(previous)), "source": grant_source, "output": next_cell})
            payload = binary(json.loads(run(args.binary, "intercell", "spore-handoff", "--request", req))["payload"])
            assert payload[:64] == original + script_hash(following)
            grant = packet(label, False, script_hash(following), nonce, wire_content=payload[64:])
            next_custody = custody_wallet(next_wallet)
            inputs = [point(last_ckb["hash"], k) for k in range(3)]
            inputs.append(authorizers[person])
            outputs = [output(mailbox_cap, ckb_escrow, mailbox), output(spore_cap, next_custody, object_type),
                       output(ckb_fees-FEE, fixture.standard), output(150*CKB, wallet)]
            def rotation(name, msg, require_holder=True):
                n = 4 if require_holder else 3
                state = le(nonce+1, 8) + consumed_root + binary(msg["record"])
                values = [hex_bytes(state), hex_bytes(data), "0x", "0x"]
                tx = transaction(inputs[:n], outputs[:n], values[:n], deps)
                tx["witnesses"] = ["0x"]*n + [cobuild(object_type, object_id, return_lock, next_custody)]
                groups = [wallet_group(2, [fixture.faucet])]
                if require_holder:
                    groups.append(wallet_group(3, [secret]))
                return fixture.sign(name, tx, groups)
            rejected[label + "_missing_ckb_owner"] = ckb_reject(rotation(label+"-no-owner", grant, False), 115)
            other = packet(label+"-other-owner", False, script_hash(fixture.standard), nonce, wire_content=payload[64:])
            rejected[label + "_ckb_recipient"] = ckb_reject(rotation(label+"-wrong-recipient", other), 113)
            evidence = submit_ckb(rotation(label, grant))
            authorizers[person] = point(evidence["hash"], 3)
            advance, votes = observe_deposit(label, evidence)
            def native_transfer(name, successor, change):
                return stem_signed(name, at(current, 2), [cell(bond_cap, endpoint), successor, change],
                    actor=previous, key=secret, bond=at(current, 0), extra_points=[source],
                    message=grant, receive=True, observations=votes)
            change = cell(native_fees-FEE, following)
            observation = {"ckb": evidence, "anchor": advance, "message": grant, "source": grant_source,
                           "successor": next_cell, "previous_custody": return_lock, "next_custody": next_custody}
            last_ckb, return_lock = evidence, next_custody
            nonce += 1
            ckb_fees -= FEE
            if i == 2:
                assert source != grant_source
                rejected["spent_handoff_source"] = stem_reject(native_transfer(label+"-old-source", next_cell, change),
                                                               native_error)
                stale_handoff = {**observation, "unspent_native_source": source}
                break
            rejected[label + "_different_capacity"] = stem_reject(native_transfer(label+"-reduced",
                cell(object_cap-1, following, data, [mapped_type]), change), native_error)
            committed = commit(native_transfer(label+"-accept", next_cell, change))
            assert signing_cell(client, at(committed, 1))["cell"] == next_cell
            handoffs.append({**observation, "native": committed})
            current = committed
            native_fees -= FEE
        return current, last_ckb, return_lock, nonce, handoffs, stale_handoff

    def export_native():
        nonlocal fee_capacity
        bond_cap = 120 * CKB
        object_cap = ((800 if approved_origin else 360 if args.native_spore_multipart else 240)
                      + (args.spore_content_bytes+99)//100) * CKB
        remaining = mint["payment_capacity"] - bond_cap - object_cap - FEE
        empty_root = bytes(32)
        permits = {}
        if issuer:
            receipt_funds = [point(funded, permit_index), point(funded, permit_index + 1)]
            receipt_capacity = 1000 * CKB

        def permit_bytes(label, source, target_cell, create=False):
            request = args.output / f"spore-permit-{label}-request.json"
            write_json(request, {"network": network, "object": hex_bytes(original), "source": source, "output": target_cell})
            encoded = json.loads(run(args.binary, "intercell", "spore-permit", "--request", request,
                                    *(["--create"] if create else [])))
            return binary(encoded["record"])

        def observe_permit(label, source, raw, evidence, index, mailbox_point=None):
            nonlocal receipt_funds
            receipt_funds = [point(evidence["hash"], i) for i in (index, index+1)]
            targets = [(receipt_funds[0], True)]
            if mailbox_point:
                targets.append((mailbox_point, False))
            anchor, votes = observe_deposit("permit-" + label, evidence, *targets)
            permits[label] = {"record": hex_bytes(raw), "issuer": issuer, "source": source,
                              "out_point": receipt_funds[0], "ckb": evidence, "stem_anchor": anchor}
            return anchor, votes

        def signed_receipt(name, record, *, both=True):
            inputs = receipt_funds if both else receipt_funds[:1]
            outputs = [output(receipt_capacity - FEE, fixture.standard, issuer)]
            values = [hex_bytes(record)]
            groups = [wallet_group(0, [fixture.faucet])]
            if both:
                outputs.append(output(150 * CKB, fixture.owner_lock))
                values.append("0x")
                groups.append(wallet_group(1, [fixture.owner]))
            tx = transaction(inputs, outputs, values, [secp, dependency(point(funded, issuer_dep_index))])
            return fixture.sign(name, tx, groups)

        def authorize(label, source, target_cell, *, create=False, mailbox_point=None):
            nonlocal receipt_capacity
            if not issuer:
                return None, None
            raw = permit_bytes(label, source, target_cell, create)
            if create:
                other_keys = sorted(map(script_hash, (fixture.standard, fixture.friend_lock)))
                other_issuer = dict(issuer, args=hex_bytes(b"".join(other_keys)))
                other = transaction([point(funded, permit_index+i) for i in (2, 3)],
                    [output(600*CKB-FEE, fixture.standard, other_issuer), output(150*CKB, fixture.friend_lock)],
                    [hex_bytes(raw), "0x"], [secp, dependency(point(funded, issuer_dep_index))])
                alternate = submit_ckb(fixture.sign("alternate-issuer", other,
                    [wallet_group(0, [fixture.faucet]), wallet_group(1, [fixture.friend])]))
                rejected["one_ckb_signer"] = ckb_reject(signed_receipt("one-signer", raw, both=False), 121)
                rejected["malformed_ckb_permit"] = ckb_reject(signed_receipt("unknown-action",
                    raw[:4] + b"\x02" + raw[5:]), 121)
            evidence = submit_ckb(signed_receipt("issue-permit-" + label, raw))
            receipt_capacity -= FEE
            anchor, votes = observe_permit(label, source, raw, evidence, 0, mailbox_point)
            if create:
                permits[label]["other_issuer_control"] = alternate
            return anchor, votes

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
        _, votes = authorize("create", mint["payment"], native_cell, create=True)
        if issuer:
            header = permits["create"]["ckb"]["inclusion"]["header"]
            other_point = point(permits["create"]["other_issuer_control"]["hash"], 0)
            other_votes = reference_votes("other-issuer", header, (other_point, True))
            rejected["other_issuer"] = stem_reject(stem_signed("other-issuer", mint["payment"],
                issued_outputs, observations=other_votes), "exited with 120")
            historical = reference_votes("historical-permit", header, (receipt_funds[0], False))
            rejected["historical_permit"] = stem_reject(stem_signed("historical-permit", mint["payment"],
                issued_outputs, observations=historical), "exited with 120")
            rejected["missing_live_permit"] = stem_reject(stem_signed("missing-reference", mint["payment"],
                issued_outputs), "exited with 120")
        issued = commit(stem_signed("issue-native", mint["payment"], issued_outputs, observations=votes))
        original_point, bond_point, fee_point = [at(issued, i) for i in range(3)]
        create_votes = votes

        native_transfers = []
        if mutant:
            rejected["local_mutant_transfer"] = stem_reject(stem_signed("local-mutant-transfer", original_point,
                [cell(object_cap-FEE, buyer, data, asset_types)]), "exited with 117")
        if approved_origin:
            recipient = buyer if issuer else owner
            gift_cell = cell(object_cap, buyer, data, asset_types)
            _, votes = authorize("gift", original_point, gift_cell)
            if issuer:
                rejected["old_anchor"] = stem_reject(stem_signed("old-anchor", fee_point,
                    [gift_cell, cell(remaining-FEE, recipient)], extra_points=[original_point],
                    observations=create_votes), "does not use the committed CKB anchor")
                for label, altered in [("other_recipient", native_cell),
                                       ("other_capacity", cell(object_cap-1, buyer, data, asset_types))]:
                    rejected[label] = stem_reject(stem_signed(label, fee_point,
                        [altered, cell(remaining-FEE, recipient)], extra_points=[original_point],
                        observations=votes), "exited with 120")
            gift = commit(stem_signed("gift", fee_point, [gift_cell, cell(remaining-FEE, recipient)],
                extra_points=[original_point], observations=votes))
            if issuer:
                rejected["previous_source"] = stem_reject(stem_signed("previous-source", at(gift, 1),
                    [gift_cell, cell(remaining-2*FEE, buyer)], extra_points=[at(gift, 0)],
                    actor=buyer, key=fixture.friend, observations=votes), "exited with 120")
            else:
                rejected["native_without_approval"] = stem_reject(stem_signed("buyer-only", at(gift, 0),
                    [cell(object_cap-FEE, owner, data, asset_types)], actor=buyer, key=fixture.friend), "exited with 115")
            remaining -= 2*FEE
            _, votes = authorize("return-gift", at(gift, 0), native_cell)
            signers = {"actor": buyer, "key": fixture.friend} if issuer else {"cosigner": (buyer, fixture.friend)}
            returned = commit(stem_signed("buyer-with-approval", at(gift, 1),
                [native_cell, cell(remaining, owner)], extra_points=[at(gift, 0)], observations=votes, **signers))
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

        def lock_asset(label, location, bond_location, fee_location, capacity, msg, votes=None):
            return stem_signed(label, fee_location, [cell(bond_cap, endpoint),
                cell(object_cap, endpoint, data, asset_types), cell(capacity - FEE, owner)],
                bond=bond_location, extra_points=[location], message=msg, observations=votes)

        if args.spore_immortal:
            rejected["native_immortal_melt"] = stem_reject(stem_signed("native-immortal-melt", original_point,
                [cell(object_cap - FEE, owner)]), "exited with 117")

        send = outbound("initial-export", bond_point)
        escrowed_cell = cell(object_cap, endpoint, data, asset_types)
        _, votes = authorize("export", original_point, escrowed_cell)
        if args.spore_content_bytes:
            for label, length, digest in [("wrong_content_hash", len(data), bytes(32)),
                                          ("wrong_content_length", len(data)-1, ckb_hash(data))]:
                changed = outbound(label, bond_point, b"SPH1"+le(length, 4)+digest)
                rejected[label] = stem_reject(lock_asset(label, original_point, bond_point, fee_point,
                                                         remaining, changed, votes), "exited with 119")
        rejected["changed_native_content"] = stem_reject(stem_signed("edited-native", original_point,
            [cell(object_cap - FEE, owner, data[:-1] + b"?", asset_types)], observations=votes), ("exited with 117", "exited with 120") if issuer else "exited with 117")
        locked = commit(lock_asset("lock-origin", original_point, bond_point, fee_point, remaining, send, votes))
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
            return sign_mint(label, tx, wallet_group(len(inputs)-1, [signer]))

        rejected["seed_without_mailbox"] = ckb_reject(mint_standard("seed-only", use_mailbox=False), 114)
        rejected["mint_without_checkpoint"] = ckb_reject(mint_standard("no-checkpoint", include_checkpoint=False), 84)
        rejected["wrong_mint_owner"] = ckb_reject(mint_standard("different-owner", recipient=friend_holder), 113)
        changed_content = data.replace(b"Stem", b"STEM", 1)
        assert changed_content != data
        rejected["changed_export_data"] = ckb_reject(mint_standard("changed-data", content=changed_content), 113)
        if not mutant:
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
                output(fee_capacity-2*FEE, fixture.standard)], [hex_bytes(data), "0x"], deps[:2] + (deps[4:] if mutant else []))
            moved["witnesses"] = ["0x", "0x", cobuild(object_type, object_id, fixture.owner_lock, fixture.friend_lock)]
            moved = submit_ckb(fixture.sign("ckb-wallet-transfer", moved,
                [wallet_group(0, [fixture.owner]), wallet_group(1, [fixture.faucet])]))
        spore, fees = [point(moved["hash"], i) for i in range(2)]

        inbound = packet("return-native", False, script_hash(owner), 0)
        return_lock = custody_wallet(fixture.owner_lock) if args.spore_handoff else ckb_escrow
        deposit_inputs = [sequencer, spore, fees]
        deposit_outputs = [output(mailbox_cap, ckb_escrow, mailbox), output(spore_cap, return_lock, object_type),
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
        def signed_deposit(name, tx_deps):
            tx = transaction(deposit_inputs, deposit_outputs, deposit_data, tx_deps)
            tx["witnesses"] = ["0x"]*len(deposit_inputs) + [cobuild(object_type, object_id, friend_holder, return_lock)]
            return fixture.sign(name, tx, deposit_signers)

        return_deps = deps
        combined = issuer and not args.separate_return_permit
        if combined:
            raw_permit = permit_bytes("restore", at(locked, 1), native_cell)
            # Both alternatives independently spend live, disjoint input sets at the same tip.
            alternatives = {
                "deposit": signed_deposit("separate-deposit-estimate", deps),
                "permit": signed_receipt("separate-permit-estimate", raw_permit),
            }
            separate_cost = {name: {"cycles": estimate(tx), "bytes": len(binary(tx["molecule"]))}
                             for name, tx in alternatives.items()}
            permit_output = len(deposit_outputs)
            deposit_signers[0]["inputs"].append(len(deposit_inputs))  # same faucet Lock as the fee input
            deposit_signers.append(wallet_group(len(deposit_inputs)+1, [fixture.owner]))
            deposit_inputs.extend(receipt_funds)
            deposit_outputs.extend([output(receipt_capacity, fixture.standard, issuer), output(150*CKB, fixture.owner_lock)])
            deposit_data.extend([hex_bytes(raw_permit), "0x"])
            return_deps = [*deps, dependency(point(funded, issuer_dep_index))]
        held = submit_ckb(signed_deposit("ckb-return", return_deps))
        if combined:
            anchor, approved = observe_permit("restore", at(locked, 1), raw_permit, held, permit_output,
                                             mailbox_point=point(held["hash"], 0))
            permits["restore"]["paired_return"] = {
                "separate": separate_cost, "combined": {key: held[key] for key in ("cycles", "bytes")}}
        elif issuer:
            anchor, approved = authorize("restore", at(locked, 1), native_cell, mailbox_point=point(held["hash"], 0))
        else:
            anchor, approved = observe_deposit("return-native", held)
        restored_outputs = [cell(bond_cap, endpoint), native_cell, cell(remaining-2*FEE, owner)]
        def restore(label, votes):
            return stem_signed(label, at(locked, 2), restored_outputs, bond=at(locked, 0),
                               extra_points=[at(locked, 1)], message=inbound, receive=True, observations=votes)
        if combined:
            header = held["inclusion"]["header"]
            partial = reference_votes("mailbox-only", header, (point(held["hash"], 0), False))
            rejected["mailbox_without_permit"] = stem_reject(restore("mailbox-only", partial), "exited with 120")
        restored = commit(restore("restore-origin", approved))
        restored_cell = cell_proof(at(restored, 1))
        assert signing_cell(client, at(restored, 1))["cell"] == native_cell

        current, last_ckb, nonce = restored, held, 1
        handoffs, stale_handoff = [], None
        if args.spore_handoff:
            current, last_ckb, return_lock, nonce, handoffs, stale_handoff = apply_handoffs(
                restored, held, nonce, binary(encoded["successor_root"]), object_cap, bond_cap,
                remaining-2*FEE, fee_capacity-3*FEE)
            remaining -= len(handoffs)*FEE
            fee_capacity -= (len(handoffs) + bool(stale_handoff))*FEE
        second = outbound("repeat-export", at(current, 0))
        _, votes = authorize("reexport", at(current, 1), escrowed_cell)
        relocked = commit(lock_asset("repeat-lock", at(current, 1), at(current, 0),
                                      at(current, 2), remaining-2*FEE, second, votes))
        next_checkpoint, next_encoded = delivery("repeat-export", second, [send["id"]], binary(encoded["successor_root"]))
        sequencer, spore, fees = [point(last_ckb["hash"], i) for i in range(3)]
        release = transaction([sequencer, spore, fees],
            [output(mailbox_cap, ckb_escrow, mailbox), output(spore_cap, owner_holder, object_type),
             output(fee_capacity-4*FEE, fixture.standard)],
            [hex_bytes(le(nonce, 8) + binary(next_encoded["successor_root"])), hex_bytes(data), "0x"],
            deps + [next_checkpoint])
        release["witnesses"] = [witness(binary(next_encoded["input_type"])), "0x", "0x",
                               cobuild(object_type, object_id, return_lock, owner_holder)]
        released = submit_ckb(fixture.sign("release-existing", release, [wallet_group(2, [fixture.faucet])]))
        final = l1.call("get_live_cell", point(released["hash"], 1), True)["cell"]
        assert final["output"] == actual["output"] and final["data"] == actual["data"]
        melt_result = None
        if args.native_spore_sync_melt:
            carried, last, fees = [point(released["hash"], i) for i in range(3)]
            refund_lock = script_hash(buyer)
            terminal = b"SPB1" + le(len(data), 4) + ckb_hash(data)
            notice = packet("synchronized-melt", False, refund_lock, nonce, wire_content=terminal)

            def certified_melt(label, msg, *, burn=True):
                inputs = [carried, last, fees]
                outputs = [output(mailbox_cap, ckb_escrow, mailbox),
                           output(spore_cap, fixture.owner_lock, None if burn else object_type),
                           output(fee_capacity-5*FEE, fixture.standard)]
                values = [hex_bytes(le(nonce+1, 8) + binary(next_encoded["successor_root"]) + binary(msg["record"])),
                          "0x" if burn else hex_bytes(data), "0x"]
                tx = transaction(inputs, outputs, values, deps)
                tx["witnesses"] = ["0x"]*3 + [cobuild(object_type, object_id, fixture.owner_lock,
                    None if burn else fixture.owner_lock)]
                return fixture.sign(label, tx, [wallet_group(1, [fixture.owner]), wallet_group(2, [fixture.faucet])])

            bad = packet("wrong-melt-hash", False, refund_lock, nonce, wire_content=terminal[:-1] + bytes([terminal[-1] ^ 1]))
            rejected["wrong_melt_content"] = ckb_reject(certified_melt("wrong-melt-hash", bad), 113)
            rejected["melt_receipt_with_live_spore"] = ckb_reject(certified_melt("still-live", notice, burn=False), 111)
            normal = packet("return-without-custody", False, refund_lock, nonce)
            rejected["melt_with_ordinary_return"] = ckb_reject(certified_melt("wrong-return", normal), 113)
            melted = submit_ckb(certified_melt("certified-melt", notice))
            assert l1.call("get_live_cell", last, True)["status"] != "live"
            terminal_anchor, votes = observe_deposit("synchronized-melt", melted)
            tag = b"SPB1" + original
            refund = cell(object_cap, buyer, tag)

            def retire(label, refund_output):
                return stem_signed(label, at(relocked, 2), [cell(bond_cap, endpoint), refund_output,
                    cell(remaining-4*FEE, owner)], bond=at(relocked, 0), extra_points=[at(relocked, 1)],
                    message=notice, receive=True, observations=votes)

            rejected["redirected_melt_refund"] = stem_reject(retire("redirect-refund",
                cell(object_cap, owner, tag)), "exited with 119")
            rejected["reduced_melt_refund"] = stem_reject(retire("reduced-refund",
                cell(object_cap-1, buyer, tag)), "exited with 119")
            rejected["untagged_melt_refund"] = stem_reject(retire("untagged-refund",
                cell(object_cap, buyer)), "exited with 119")
            retired = commit(retire("retire-original", refund))
            final_original = cell_proof(at(relocked, 1))
            assert final_original["cell"] is None and signing_cell(client, at(retired, 1))["cell"] == refund
            rejected["replayed_terminal_receipt"] = stem_reject(stem_signed("replay-terminal", at(retired, 2),
                [cell(bond_cap, endpoint), cell(remaining-5*FEE, owner)], bond=at(retired, 0),
                message=notice, receive=True, observations=votes), "nonce already used")
            melt_result = {"transaction": melted, "receipt": notice, "anchor": terminal_anchor,
                           "retired": retired, "spent_original": final_original, "refund": cell_proof(at(retired, 1)),
                           "authenticated_inbox": certified("/stem/intercell/inbox", notice["id"])}
        elif not args.spore_immortal and not approved_origin and not mutant:
            last = point(released["hash"], 1)
            melt = transaction([last], [output(spore_cap - FEE, fixture.owner_lock)], ["0x"], deps[:2])
            melt["witnesses"] = ["0x", cobuild(object_type, object_id, fixture.owner_lock, None)]
            melted = submit_ckb(fixture.sign("standard-melt", melt, [wallet_group(0, [fixture.owner])]))
            assert l1.call("get_live_cell", last, True)["status"] != "live"
            held_native = cell_proof(at(relocked, 1))
            assert signing_cell(client, at(relocked, 1))["cell"] == cell(object_cap, endpoint, data, asset_types)
            melt_result = {"transaction": melted, "native_custody": held_native}
        steps = {"issue": issued, "lock": locked, "restore": restored, "relock": relocked}
        steps.update({f"handoff-{i+1}": step["native"] for i, step in enumerate(handoffs)})
        if args.native_spore_sync_melt:
            steps["retire"] = retired
        if approved_origin:
            steps.update(zip(("gift", "return-gift"), native_transfers))
        return {**({"permits": permits} if issuer else {}), **({"mutant_registration": mutant_registration} if mutant else {}),
                **({"handoffs": handoffs} if handoffs else {}),
                **({"stale_handoff": stale_handoff} if stale_handoff else {}),
                **({"cluster": cluster} if cluster else {}),
                "execution": {label: execution(tx) for label, tx in steps.items()},
                "origin": "stem", "immortal": args.spore_immortal, "spore_id": hex_bytes(object_id), "spore_type": object_type,
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

    entry_lock = custody_wallet(fixture.owner_lock) if args.spore_handoff else ckb_escrow

    def deposit(label, *, custodian=entry_lock, move=True, content=data, policy=importing,
                from_lock=fixture.owner_lock, key=fixture.owner, consumed=empty, include_cobuild=True):
        inputs = [mailbox_point, object_point, l1_fee_point] if move else [mailbox_point, l1_fee_point]
        outputs = [output(mailbox_cap, ckb_escrow, mailbox)]
        payload = [hex_bytes(le(nonce_frontier + 1, 8) + consumed + binary(policy["record"]))]
        if move:
            outputs.append(output(spore_cap, custodian, object_type))
            payload.append(hex_bytes(content))
        outputs.append(output(fee_capacity - FEE, fixture.standard))
        tx = transaction(inputs, outputs, payload + ["0x"], deps)
        if move:
            if include_cobuild:
                tx["witnesses"] = ["0x"] * len(inputs) + [
                    cobuild(object_type, object_id, from_lock, custodian)]
            groups = [wallet_group(1, [key]), wallet_group(2, [fixture.faucet])]
        else:
            groups = [wallet_group(1, [fixture.faucet])]
        return fixture.sign(f"spore-{label}", tx, groups)

    if args.spore_content_bytes:
        for label, length, digest in [("ckb_wrong_content_hash", len(data), bytes(32)),
                                      ("ckb_wrong_content_length", len(data)-1, ckb_hash(data))]:
            msg = packet(label, False, script_hash(owner), 0, wire_content=b"SPH1"+le(length, 4)+digest)
            rejected[label] = ckb_reject(deposit(label, policy=msg), 113)
    if args.spore_handoff:
        mime, body, cluster_field = [int.from_bytes(data[i:i+4], "little") for i in (4, 8, 12)]
        content = data[body+4:cluster_field]
        for label, alternative in (("changed_content_byte", bytes([content[0] ^ 1]) + content[1:]),
                                   ("shortened_content", content[:-1])):
            changed = table(data[mime:body], molecule_bytes(alternative), data[cluster_field:])
            advertised = packet(label, False, script_hash(owner), 0, changed)
            rejected[label] = ckb_reject(deposit(label, content=changed, policy=advertised), 112)
            assert escrow_code.hex() in rejected[label], rejected[label]
    held_signed = deposit("deposit")
    held_cycles = estimate(held_signed)
    rejected["ckb_deposit_without_original"] = ckb_reject(deposit("fake-deposit", move=False), 111)
    if not mutant:
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
    try:
        duplicate = stem_signed("duplicate-import", funding,
            [cell(bond, endpoint), mapped, mapped, cell(source_cap - mapped_cap * 2 - FEE, owner)],
            bond=bond_at, message=importing, receive=True, observations=approved)
    except subprocess.CalledProcessError as error:
        if error.cmd[1:3] != ["wallet", "build"] or "Transaction exceeds byte limit" not in (error.stderr or ""):
            raise
        rejected["wallet_oversized_duplicate"] = error.stderr.strip()
    else:
        rejected["stem_duplicate_output"] = stem_reject(duplicate, "exited with 109")
    imported = commit(stem_signed("import", funding, [cell(bond, endpoint), mapped, change],
        bond=bond_at, message=importing, receive=True, observations=approved))
    bond_at, mapped_at, funding = at(imported, 0), at(imported, 1), at(imported, 2)
    rejected["stem_import_replay"] = stem_reject(stem_signed("import-replay", funding,
        [cell(bond, endpoint), mapped, cell(source_cap - mapped_cap * 2 - 2 * FEE, owner)],
        bond=bond_at, message=importing, receive=True, observations=approved), "nonce already used")

    handoffs, stale_handoff = [], None
    return_lock = entry_lock
    if args.spore_handoff:
        current, last_ckb, return_lock, nonce_frontier, handoffs, stale_handoff = apply_handoffs(
            imported, held, nonce_frontier, empty, mapped_cap, bond, source_cap-mapped_cap-FEE, fee_capacity)
        bond_at, mapped_at, funding = [at(current, i) for i in range(3)]
        mailbox_point, object_point, l1_fee_point = [point(last_ckb["hash"], i) for i in range(3)]
        source_cap -= len(handoffs)*FEE
        fee_capacity -= (len(handoffs) + bool(stale_handoff))*FEE

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
        if mutant:
            deps[deps.index(dependency(cluster_point))] = dependency(point(cluster["transferred"]["hash"], 0))
        l1_fee_point = point(cluster["transferred"]["hash"], 1)
        fee_capacity -= FEE
        assert l1.call("get_live_cell", cluster_point, True)["status"] != "live"
    candidate = stem_signed("transfer", mapped_at, [cell(mapped_cap - FEE, buyer, data, [mapped_type])])
    if mutant:
        rejected["local_mutant_transfer"] = stem_reject(candidate, "exited with 107")
        transferred = None
        holder, holder_key = owner, fixture.owner
    else:
        transferred = commit(candidate)
        mapped_at = at(transferred, 0)
        holder, holder_key = buyer, fixture.friend
    release_lock = fixture.owner_lock if mutant else fixture.friend_lock
    nonce = int.from_bytes(ckb_hash(b"Stem Spore return nonce\0" + binary(bond_at["tx_hash"])
                                  + le(int(bond_at["index"], 16), 4))[:8], "little")
    returning = packet("return", True, script_hash(release_lock), nonce)
    returned_outputs = [cell(bond, endpoint), cell(mapped_cap - (1 if mutant else 2) * FEE, holder)]
    rejected["stem_burn_without_return"] = stem_reject(stem_signed("no-return", mapped_at,
        [cell(mapped_cap - (1 if mutant else 2) * FEE, holder)], actor=holder, key=holder_key), "exited with 107")
    rejected["stem_return_without_object"] = stem_reject(stem_signed("return-without-object", funding,
        [cell(bond, endpoint), cell(source_cap - mapped_cap - 2 * FEE, owner)],
        bond=bond_at, message=returning), "exited with 109")
    rejected["stem_return_to_ckb_escrow"] = stem_reject(stem_signed("return-to-ckb-escrow", mapped_at,
        returned_outputs, actor=holder, key=holder_key, bond=bond_at,
        message=packet("return-to-ckb-escrow", True, script_hash(ckb_escrow), nonce)), "exited with 108")
    burned = commit(stem_signed("return", mapped_at, returned_outputs,
        actor=holder, key=holder_key, bond=bond_at, message=returning))

    checkpoint, encoded = delivery("outbox", returning, [], empty)
    path = binary(encoded["input_type"])

    def redeem(label, recipient=release_lock, *, include_checkpoint=True, capacity=spore_cap):
        tx = transaction([mailbox_point, object_point, l1_fee_point],
            [output(mailbox_cap, ckb_escrow, mailbox), output(capacity, recipient, object_type),
             output(fee_capacity - FEE + spore_cap - capacity, fixture.standard)],
            [hex_bytes(le(nonce_frontier, 8) + binary(encoded["successor_root"])), hex_bytes(data), "0x"],
            deps + ([checkpoint] if include_checkpoint else []))
        tx["witnesses"] = [witness(path), "0x", "0x",
                           cobuild(object_type, object_id, return_lock, recipient)]
        return fixture.sign(f"spore-{label}", tx, [wallet_group(2, [fixture.faucet])])

    rejected["ckb_release_without_checkpoint"] = ckb_reject(redeem("no-checkpoint", include_checkpoint=False), 84)
    rejected["ckb_release_wrong_owner"] = ckb_reject(redeem("wrong-owner", recipient=fixture.friend_lock if mutant else fixture.owner_lock), 113)
    rejected["ckb_release_capacity_drain"] = ckb_reject(redeem("capacity-drain", capacity=spore_cap - CKB), 112)
    released = submit_ckb(redeem("release"))
    final = l1.call("get_live_cell", point(released["hash"], 1), True)["cell"]
    assert final["output"] == output(spore_cap, release_lock, object_type)
    assert final["data"]["content"] == hex_bytes(data)
    assert l1.call("get_live_cell", object_point, True)["status"] != "live"
    end_cell = cell_proof(mapped_at)
    assert end_cell["cell"] is None

    mailbox_point, object_point, l1_fee_point = [point(released["hash"], i) for i in range(3)]
    fee_capacity -= FEE
    wallet_move = None
    if mutant:
        move = transaction([object_point, l1_fee_point],
            [output(spore_cap, fixture.friend_lock, object_type), output(fee_capacity-FEE, fixture.standard)],
            [hex_bytes(data), "0x"], deps)
        move["witnesses"] = ["0x", "0x", cobuild(object_type, object_id, release_lock, fixture.friend_lock)]
        wallet_move = submit_ckb(fixture.sign("mutant-wallet-transfer", move, [
            wallet_group(0, [fixture.owner]), wallet_group(1, [fixture.faucet])]))
        object_point, l1_fee_point = [point(wallet_move["hash"], i) for i in range(2)]
        fee_capacity -= FEE
    redeposit_message = packet("redeposit", False, script_hash(buyer), nonce_frontier)
    redeposited = submit_ckb(deposit("redeposit", custodian=ckb_escrow, policy=redeposit_message, from_lock=fixture.friend_lock,
                                   key=fixture.friend, consumed=binary(encoded["successor_root"])))
    mailbox_point, object_point, l1_fee_point = [point(redeposited["hash"], i) for i in range(3)]
    fee_capacity -= FEE
    nonce_frontier += 1
    return_lock = ckb_escrow
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


    return {**({"mutant_registration": mutant_registration, "ckb_wallet_transfer": wallet_move} if mutant else {}),
            "id": hex_bytes(object_id), "immortal": args.spore_immortal, "original_type": object_type, "mapped_type": mapped_type, "cluster": cluster,
            "mailbox_creation": l1.prove_committed(created), "mailbox": mailbox, "endpoint": endpoint,
            **({"handoffs": handoffs} if handoffs else {}),
            **({"stale_handoff": stale_handoff} if stale_handoff else {}),
            "minted": minted, "held": held, "anchor": anchor, "imported": imported,
            "transferred": transferred, "burned": burned, "released": released, "final_ckb": final,
            "locked_bond": signing_cell(client, at(reimported, 0)), "spent_mapping": end_cell,
            "outbox": certified("/stem/intercell/outbox", returning["id"]),
            "inbox": certified("/stem/intercell/inbox", importing["id"]),
            "redeposited": redeposited, "second_anchor": second_anchor,
            "reimported": reimported, "reimported_cell": revived_cell,
            "reimported_proof": cell_proof(at(reimported, 1)),

            "execution": {name: execution(tx) for name, tx in (
                ("imported", imported), ("transferred", transferred), ("burned", burned), ("reimported", reimported),
                *((f"handoff-{i+1}", step["native"]) for i, step in enumerate(handoffs))) if tx is not None},
            "rejections": rejected}

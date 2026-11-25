#!/usr/bin/env python3
"""Exercise and reconcile Stem's native bridge against locally deployed CKB vaults."""

import argparse
import base64
import copy
import json
from pathlib import Path
import secrets
import subprocess
import time

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes,
    le, output, packed_script, point, reject_cli, script, script_hash, transaction,
    vault_state, wallet_group, write_key,
)
from direct_checkpoint_localnet import Relay
from localnet import Rpc, comet_app_command, run, write_json
from stake_localnet import witness


def native_script(lock):
    args = binary(lock["args"])
    address = {"data": 0, "type": 1}[lock["hash_type"]]
    return binary(lock["code_hash"]) + bytes([address]) + le(len(args), 4) + args


def cell(capacity, lock, data=b"", types=()):
    scripts = sorted(map(native_script, types))
    items = le(len(scripts), 4) + b"".join(scripts) if scripts else b""
    return hex_bytes(bytes([bool(scripts)]) + le(capacity, 8) + native_script(lock)
                     + items + le(len(data), 4) + data)


def stem_id(args, network, tx):
    command = [str(args.binary), "wallet", "id", "--network", network]
    return json.loads(subprocess.check_output(command, input=json.dumps(tx).encode()))


def signing_cell(rpc, out_point):
    packed = binary(out_point["tx_hash"]) + le(int(out_point["index"], 16), 4)
    response = rpc.call("abci_query", path=json.dumps("/stem/cell"), data=hex_bytes(packed))["response"]
    if response["code"] != 0:
        raise RuntimeError(f"read signing Cell: {response}")
    record = bytes.fromhex(json.loads(base64.b64decode(response["value"]))["cell"])
    if record[:36] != packed or len(record) <= 84:
        raise RuntimeError("invalid signing Cell record")
    return {"out_point": out_point, "cell": hex_bytes(record[84:])}


def start(args, command, file):
    log = (args.output / file).open("wb", buffering=0)
    try:
        return subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
    finally:
        log.close()


def wait_for(func, label, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = func()
            if result:
                return result
        except (OSError, RuntimeError):
            pass
        time.sleep(.08)
    raise RuntimeError(f"timed out waiting for {label}")


def deploy(args, l1, fixture, network, funding_point=None, checkpoint_anchor=None, *, prefix=""):
    genesis = l1.call("get_block_by_number", "0x0")
    faucet, groups = genesis["transactions"][:2]
    index, funds = next((i, out) for i, out in enumerate(faucet["outputs"]) if out["lock"]["args"] == FAUCET_ARG)
    secp = dependency(point(groups["hash"], 0), "dep_group")
    multisig = dependency(point(groups["hash"], 1), "dep_group")
    multisig_code = hex_bytes(script_hash(faucet["outputs"][4]["type"]))
    custody = script(multisig_code, hex_bytes(ckb_hash(fixture.multisig_prefix)[:20]))
    if funding_point is None:
        funding_point = point(faucet["hash"], index)
    else:
        previous = l1.call("get_transaction", funding_point["tx_hash"])["transaction"]
        funds = previous["outputs"][int(funding_point["index"], 16)]
    codes = [args.vault.read_bytes(), args.deposit_lock.read_bytes()]
    amounts = [(len(code) + 200) * CKB for code in codes]
    reference = getattr(args, "reference_script", None)
    programs = len(args.native_lock.read_bytes()) + (len(args.wrapped.read_bytes()) if args.xudt else 0)
    if reference:
        programs += len(reference.read_bytes())
    funding = (programs + 20_000) * CKB
    authorization = 300 * CKB
    change = int(funds["capacity"], 16) - sum(amounts) - funding - authorization - FEE
    checkpoint_outputs, checkpoint_data = [], []
    if checkpoint_anchor is None:
        checkpoint = args.direct_checkpoint.read_bytes()
        checkpoint_capacity, relay_funding = (len(checkpoint) + 200) * CKB, 8_000 * CKB
        change -= checkpoint_capacity + 2 * relay_funding
        checkpoint_outputs = [output(checkpoint_capacity, fixture.standard), output(relay_funding, fixture.standard)]
        checkpoint_data = [hex_bytes(checkpoint), "0x"]
    custody_index = 4 + len(checkpoint_outputs)
    outputs = [output(c, fixture.standard) for c in amounts + [funding, change]]
    outputs += checkpoint_outputs + [output(authorization, custody)]
    data = [hex_bytes(code) for code in codes] + ["0x", "0x"] + checkpoint_data + ["0x"]
    if checkpoint_anchor is None:
        outputs.append(output(relay_funding, fixture.standard))
        data.append("0x")
    deployment = transaction(
        [funding_point], outputs, data, [secp],
    )
    deployed = l1.send(fixture.sign(f"{prefix}deploy", deployment, [wallet_group(0, [fixture.faucet])]))
    vault_code = dependency(point(deployed, 0))
    deposit_code = dependency(point(deployed, 1))
    deposit_hash = hex_bytes(ckb_hash(codes[1]))
    type_id = hex_bytes(ckb_hash(le(0, 8) + binary(deployed) + le(2, 4) + le(0, 8)))
    anchor = checkpoint_anchor
    foreign_anchor = None
    if anchor is None:
        anchor = {
            "hash": hex_bytes(ckb_hash(checkpoint)), "id": ckb_hash(le(0, 8) + binary(deployed) + le(5, 4) + le(0, 8)),
            "code": dependency(point(deployed, 4)), "seed": point(deployed, 5),
            "network": network, "genesis": genesis["header"]["hash"],
        }
        foreign_anchor = dict(anchor, id=ckb_hash(le(0, 8) + binary(deployed) + le(7, 4) + le(0, 8)),
                              seed=point(deployed, 7))
    assert anchor["network"] == network and anchor["genesis"] == genesis["header"]["hash"]
    policy = binary(anchor["hash"]) + anchor["id"] + binary(anchor["genesis"]) + binary(network)
    vault_type = script(hex_bytes(ckb_hash(codes[0])), hex_bytes(binary(type_id) + policy), "data2")
    vault_hash = hex_bytes(script_hash(vault_type))
    lock = dict(vault_type, args=hex_bytes(binary(vault_hash) + binary(custody["args"])))
    reserve = 1600 * CKB
    initial = vault_state(0, 0, deposit_hash, multisig_code)
    tx = transaction([point(deployed, 2)], [output(reserve, lock, vault_type),
                     output(funding - reserve - FEE, fixture.standard)], [initial, "0x"], [vault_code, secp])
    vault = l1.send(fixture.sign(f"{prefix}vault", tx, [wallet_group(0, [fixture.faucet])]))
    return {
        "genesis": genesis["header"]["hash"], "vault": vault, "state": initial, "type": vault_type,
        "vault_hash": vault_hash, "lock": lock, "reserve": reserve, "funding": funding - reserve - FEE,
        "secp": secp, "multisig": multisig, "vault_code": vault_code, "deposit_code": deposit_code,
        "change_point": point(deployed, 3), "change": change, "anchor": anchor,
        "custody": custody, "custody_cell": point(deployed, custody_index), "authorization": authorization,
        "foreign_anchor": foreign_anchor,
    }


def native_spec(args, l1, fixture, vault, token):
    comet = args.output / "comet"
    run(args.comet, "init", "--home", comet)
    comet_config = comet / "config/genesis.json"
    config = json.loads(comet_config.read_text())
    config["chain_id"] = "stem-native-ckb-local"
    config["genesis_time"] = "2025-01-01T00:00:00Z"
    network = vault["anchor"]["network"]
    bridge, lane = bridge_configuration(args, l1, fixture, vault, token, network)
    spec = {
        "chain_id": config["chain_id"], "network_id": network, "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024,
                   "transaction_cycles": 20_000_000, "block_cycles": 400_000_000,
                   "cell_reads": 1024, "cell_read_bytes": 64 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "development_validators": [
            {"ed25519": hex_bytes(base64.b64decode(member["pub_key"]["value"])), "power": int(member["power"])}
            for member in config["validators"]
        ],
        "bridge": bridge,
    }
    spec_path = args.output / "stem.json"
    write_json(spec_path, spec)
    latest = json.loads(run(args.binary, "genesis-hash", "--genesis", spec_path))
    config["app_state"] = {"stem_genesis": latest.removeprefix("0x")}
    write_json(comet_config, config)
    return comet, spec, spec_path, lane


def bridge_configuration(args, l1, fixture, vault, token, network):
    lane = hex_bytes(ckb_hash(b"Stem bridge lane\0" + binary(vault["genesis"]) + binary(vault["vault_hash"]) + b"\0"))
    pinned = int(l1.call("get_tip_block_number"), 16)
    block = l1.call("get_block_hash", hex(pinned))
    members = sorted(fixture.guardians, key=lambda member: member["ed25519"])
    burn = hex_bytes(ckb_hash(b"Stem native test Burn Lock\0"))
    wrapped = hex_bytes(ckb_hash(args.wrapped.read_bytes()))
    maximum = {"operations": 8, "bytes": 2 * 1024 * 1024, "signatures": 32}
    bridge = {
        "l1_genesis": vault["genesis"], "confirmations": 2, "anchor_height": pinned,
        "anchor_hash": block, "guardian_generation": hex_bytes(ckb_hash(b"Stem CKB bridge fixtures" + binary(network))),
        "guardians": members, "wrapped_code_hash": wrapped, "burn_code_hash": burn,
        "lanes": [{"vault_type": vault["type"], "vault": point(vault["vault"], 0)}],
        "recipient_locks": [{"example": fixture.friend_lock}],
        "reserved_recipient_locks": [vault["lock"]],
        "mint_limits": maximum, "settlement_limits": maximum,
        "anchor_limits": {"operations": 1, "bytes": 65536, "signatures": 32},
        "deposits_per_block": 32,
    }
    if token:
        bridge["lanes"].append({
            "vault_type": token["type"], "vault": point(token["vault"], 0), "l1_type": token["l1_type"],
        })
        bridge["reserved_recipient_locks"].append(token["escrow_lock"])
    return bridge, lane


def deposit_and_mint(args, l1, fixture, vault, spec, spec_path, lane, comet_rpc):
    program = args.native_lock.read_bytes()
    lock = script(hex_bytes(ckb_hash(program)), fixture.key(fixture.owner)["blake160"], "data")
    payloads = [cell((len(program) + 500) * CKB, lock, program), cell(1200 * CKB, lock)]
    if args.xudt:
        wrapper = args.wrapped.read_bytes()
        payloads.append(cell((len(wrapper) + 500) * CKB, lock, wrapper))
    reference = getattr(args, "reference_script", None)
    if reference:
        code = reference.read_bytes()
        payloads.append(cell((len(code) + 500) * CKB, lock, code))
    body = []
    for index, payload in enumerate(payloads):
        path = args.output / f"deposit-cell-{index}.json"
        write_json(path, payload)
        body.append(json.loads(run(args.binary, "bridge", "deposit", "--genesis", spec_path,
                                   "--l1-rpc", l1.url, "--lane", lane, "--cell", path)))
    total = sum(item["capacity"] for item in body)
    change = vault["funding"] - total - FEE
    tx = transaction([point(vault["vault"], 1)],
                     [output(item["capacity"], item["lock"]) for item in body] + [output(change, fixture.standard)],
                     [item["data"] for item in body] + ["0x"], [vault["secp"]])
    deposited = l1.send(fixture.sign("deposits", tx, [wallet_group(0, [fixture.faucet])]))
    consolidation = transaction(
        [point(vault["vault"], 0)] + [point(deposited, index) for index in range(len(body) + 1)] + [vault["custody_cell"]],
        [output(vault["reserve"] + total, vault["lock"], vault["type"]), output(change - FEE, fixture.standard),
         output(vault["authorization"], vault["custody"])],
        [vault["state"], "0x", "0x"], [vault["vault_code"], vault["deposit_code"], vault["multisig"], vault["secp"]],
    )
    groups = [wallet_group(len(body) + 2, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
              wallet_group(len(body) + 1, [fixture.faucet])]
    consolidated = l1.send(fixture.sign("consolidate", consolidation, groups))
    vault["custody_cell"] = point(consolidated, 2)
    encoded, minted, approvals, rejected = attain_mint(
        args, l1, fixture, vault, spec_path, lane, consolidated, comet_rpc, "ckb",
        spec["bridge"]["anchor_height"], spec["bridge"]["anchor_hash"], [],
    )
    # The confirmed L1 funds back both minted Cells, including the native Lock Code Cell.
    mint_id = stem_id(args, spec["network_id"], encoded)
    query = comet_rpc.call("abci_query", path=json.dumps("/stem/cell"),
                           data=hex_bytes(binary(mint_id) + le(1, 4)), prove="true")["response"]
    if query["code"] != 0:
        raise AssertionError(query)
    proof = json.loads(base64.b64decode(query["value"]))
    if not proof["cell"] or not proof["proof"]:
        raise AssertionError("native mint did not create the funded Live Cell and proof")
    return {"l1_consolidation": consolidated, "l1_proof": l1.prove_committed(consolidated),
            "l1_deposits": deposited, "mint": minted, "mint_id": mint_id, "capacity": total,
            "mint_proof": proof, "minority_rejected": rejected, "payment_capacity": 1200 * CKB,
            "lock_dep": point(mint_id, 0), "payment": point(mint_id, 1),
            "confirmed_height": approvals[0]["mint"]["confirmed_height"],
            "confirmed_hash": approvals[0]["mint"]["confirmed_hash"]}


def attain_mint(args, l1, fixture, vault, spec_path, lane, consolidated, comet_rpc, name,
                prior_height, prior_hash, deps):
    request = args.output / f"{name}-mint-request.json"
    write_json(request, {"lane": lane, "vault_input": point(vault["vault"], 0), "consolidation": consolidated,
                         "previous_l1_height": prior_height, "previous_l1_hash": prior_hash, "cell_deps": deps})
    l1.mine(2)
    approvals = []
    for index, key in enumerate(fixture.guardian_keys):
        signed = run(args.binary, "bridge", "attest-mint", "--genesis", spec_path, "--l1-rpc", l1.url,
                     "--request", request, "--journal", args.output / f"journal-{index}", "--key-file", key)
        approvals.append(json.loads(signed))
    approval_file = args.output / f"{name}-approvals.json"
    write_json(approval_file, approvals[:2])
    minority = json.loads(run(args.binary, "bridge", "mint", "--observations", approval_file))
    rejected = comet_rpc.call("broadcast_tx_sync", tx=minority)
    if rejected["code"] == 0:
        raise AssertionError("Stem accepted a minority mint certificate")
    write_json(approval_file, approvals[:3])
    encoded = json.loads(run(args.binary, "bridge", "mint", "--observations", approval_file))
    minted = comet_rpc.call("broadcast_tx_commit", tx=encoded)
    if minted["check_tx"]["code"] != 0 or int(minted["height"]) == 0:
        raise AssertionError(minted)
    return encoded, minted, approvals, rejected


def withdraw_and_settle(args, l1, fixture, vault, spec, spec_path, lane, mint, comet_rpc, relay,
                        *, typed=None):
    source = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())), fixture.key(fixture.owner)["blake160"], "data")
    recipient = fixture.friend_lock
    burn_lock = script(spec["bridge"]["burn_code_hash"],
                       hex_bytes(binary(lane) + packed_script(recipient)), "data")
    name = "xudt" if typed else "ckb"
    capacity = mint["payment_capacity"] - FEE
    types = [typed["wrapped"]] if typed else []
    units = le(typed["amount"], 16) if typed else b""
    deps = [dependency(mint["lock_dep"])] + ([dependency(typed["stem_dep"])] if typed else [])
    build = args.output / f"{name}-burn.json"
    write_json(build, {"inputs": [{"out_point": mint["payment"]}],
                       "outputs": [cell(capacity, burn_lock, units, types)],
                       "cell_deps": deps})
    draft = args.output / f"{name}-burn-draft.json"
    write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", build)))
    lock_path = args.output / "native-owner-lock.json"
    write_json(lock_path, source)
    context = args.output / f"{name}-burn-signing-context.json"
    write_json(context, {"cells": [signing_cell(comet_rpc, point)
                                   for point in [mint["payment"], *(dep["out_point"] for dep in deps)]]})
    signed = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
                            "--transaction", draft, "--context", context, "--group-index", "0", "--lock", lock_path,
                            "--key-file", fixture.owner))
    burned = comet_rpc.call("broadcast_tx_commit", tx=signed)
    return redeem_withdrawal(
        args, l1, fixture, vault, spec, spec_path, lane, comet_rpc, relay,
        name=name, burned=burned, recipient=recipient, capacity=capacity, units=units,
        nonce=0, input_vault=point(mint["l1_consolidation"], 0),
        fee_input=point(mint["l1_consolidation"], 2 if typed else 1),
        previous_anchor=mint, typed=typed, probe_checkpoints=True,
    )


def redeem_withdrawal(args, l1, fixture, vault, spec, spec_path, lane, comet_rpc, relay, *,
                      name, burned, recipient, capacity, units, nonce, input_vault,
                      fee_input, previous_anchor, typed=None, probe_checkpoints=False):
    height = int(burned["height"])
    if not height or burned["check_tx"]["code"] != 0:
        raise AssertionError(burned)
    tx_location = wait_for(lambda: comet_rpc.call("tx", hash="0x" + burned["hash"].removeprefix("0x")),
                           "burn transaction index")
    index = int(tx_location["index"])
    wait_for(lambda: int(comet_rpc.call("status")["sync_info"]["latest_block_height"]) > height,
             "Stem successor block")

    def query(path, **params):
        response = comet_rpc.call("abci_query", path=json.dumps(path), height=height, **params)["response"]
        if response["code"] != 0:
            raise AssertionError(response)
        return base64.b64decode(response["value"])

    header = query("/stem/header")
    proof = json.loads(query("/stem/execution", data=hex_bytes(le(index, 4)), prove="true"))
    proof["transaction"] = hex_bytes(bytes.fromhex(proof["transaction"]))
    proof["receipt"] = hex_bytes(bytes.fromhex(proof["receipt"]))
    commit, finality_arguments, verification, handoffs = relay.audit_burn(f"{name}-burn", height, header)
    fee_cell = l1.call("get_live_cell", fee_input, True)["cell"]["output"]
    before = int(l1.call("get_live_cell", input_vault, True)["cell"]["output"]["capacity"], 16)
    response = comet_rpc.call(
        "abci_query", path=json.dumps("/stem/bridge-withdrawal"), prove="true",
        data=hex_bytes(binary(lane) + le(nonce, 8)),
    )["response"]
    pending = json.loads(base64.b64decode(response["value"]))
    pending_height = int(response["height"])
    if response["code"] != 0 or pending_height < height or not pending["record"]:
        raise AssertionError(response)
    wait_for(lambda: int(comet_rpc.call("status")["sync_info"]["latest_block_height"]) > pending_height,
             "Stem successor for the pending SystemRoot")
    pending_proof = args.output / f"{name}-pending-proof.json"
    pending_header = args.output / f"{name}-pending-header.json"
    write_json(pending_proof, pending)
    write_json(pending_header, hex_bytes(relay.header(pending_height)))
    payload = binary(json.loads(run(
        args.binary, "bridge", "withdrawal-witness", "--genesis", spec_path, "--lane", lane,
        "--nonce", str(nonce), "--header", pending_header, "--proof", pending_proof,
    )))
    l1_claim = binary(pending["record"])[:32]
    previous = binary(vault["state"])
    state = binary(vault_state(1, nonce + 1, hex_bytes(previous[10:42]), hex_bytes(previous[42:74]),
                              amount=capacity, payout_hash=script_hash(recipient), burn_id=l1_claim))
    inputs = [input_vault]
    outs = [output(capacity, recipient, vault["l1_type"] if typed else None),
            output(before - capacity, vault["lock"], vault["type"])]
    deps = [vault["vault_code"], vault["secp"]]
    if typed:
        state = bytes([2]) + state[1:] + previous[146:210] + units
        inputs.append(point(input_vault["tx_hash"], 1))
        outs.append(output(vault["escrow_capacity"], vault["escrow_lock"], vault["l1_type"]))
        deps.extend([vault["gate_dep"], vault["token_dep"]])
    data = [hex_bytes(units), hex_bytes(state)] + ([hex_bytes(le(0, 16))] if typed else [])
    inputs.append(fee_input)
    outs.append(output(int(fee_cell["capacity"], 16) - FEE, fixture.standard))

    def release(label, checkpoint, claim=payload, modify=None):
        tx = transaction(inputs, copy.deepcopy(outs), data + ["0x"], deps + ([checkpoint] if checkpoint else []))
        if modify:
            modify(tx)
        tx["witnesses"] = [witness(claim)]
        return fixture.sign(f"{name}-{label}", tx, [wallet_group(len(inputs) - 1, [fixture.faucet])])

    def wrong_recipient(tx):
        other = fixture.owner_lock if recipient != fixture.owner_lock else fixture.friend_lock
        tx["outputs"][0]["lock"] = other
        after = binary(tx["outputs_data"][1])
        tx["outputs_data"][1] = hex_bytes(after[:82] + script_hash(other) + after[114:])

    pre_checkpoint = dependency(relay.point)
    negative = {}
    if probe_checkpoints:
        negative["old_system_root"] = l1.reject(release("early-release", pre_checkpoint))
    checkpoint = relay.advance(pending_height, vault["secp"])
    negative["missing_checkpoint"] = l1.reject(release("no-checkpoint", None))
    negative["modified_path"] = l1.reject(release("bad-path", checkpoint, payload[:-1] + bytes([payload[-1] ^ 1])))
    negative["wrong_recipient"] = l1.reject(release("wrong-recipient", checkpoint, modify=wrong_recipient))
    foreign_checkpoint = None
    if typed:
        def wrong_units(tx):
            other_units = typed["amount"] - 1
            tx["outputs_data"][0] = hex_bytes(le(other_units, 16))
            tx["outputs_data"][1] = hex_bytes(state[:210] + le(other_units, 16))
            tx["outputs_data"][2] = hex_bytes(le(1, 16))

        negative["wrong_xudt_units"] = l1.reject(release("wrong-units", checkpoint, modify=wrong_units))
    elif probe_checkpoints:
        foreign = relay.foreign(vault["foreign_anchor"])
        foreign_dep = foreign.advance(pending_height, vault["secp"])
        foreign_checkpoint = {"type": foreign.type, "publications": foreign.publications}
        if foreign.type == relay.type:
            raise AssertionError("fixture derived duplicate checkpoint identities")
        negative["foreign_checkpoint_type_id"] = l1.reject(release("foreign-checkpoint", foreign_dep))
    signed_release = release("release", checkpoint)
    estimate = int(l1.call("estimate_cycles", signed_release["json"])["cycles"], 16)
    payout = l1.send(signed_release)
    if not typed and probe_checkpoints:
        fee = int(outs[-1]["capacity"], 16)
        replay_after = state[:2] + le(nonce + 2, 8) + state[10:]
        replay = transaction([point(payout, 1), point(payout, 2)],
                             [outs[0], output(before - 2 * capacity, vault["lock"], vault["type"]),
                              output(fee - FEE, fixture.standard)],
                             ["0x", hex_bytes(replay_after), "0x"], deps + [checkpoint])
        replay["witnesses"] = [witness(payload)]
        negative["old_nonce"] = l1.reject(fixture.sign("ckb-replay-withdrawal", replay, [wallet_group(1, [fixture.faucet])]))
    l1.mine(spec["bridge"]["confirmations"])
    request = {
        "lane": lane, "nonce": nonce, "payout": payout, "vault_input": input_vault,
        "previous_l1_height": previous_anchor["confirmed_height"], "previous_l1_hash": previous_anchor["confirmed_hash"],
        "header": hex_bytes(header), "commit": commit.name, **proof,
    }
    if handoffs is not None:
        request["handoffs"] = handoffs.name
    request_file = args.output / f"{name}-payout-request.json"
    write_json(request_file, request)
    ancestry_rejection = None
    if not finality_arguments and probe_checkpoints:
        bundle = json.loads(commit.read_text())
        frontier = bytearray(binary(bundle["history"]["frontier"]))
        frontier[-1] ^= 1
        bundle["history"]["frontier"] = hex_bytes(frontier)
        bad_finality = args.output / f"{name}-bad-finality.json"
        bad_request = args.output / f"{name}-bad-ancestry-request.json"
        write_json(bad_finality, bundle)
        write_json(bad_request, dict(request, commit=bad_finality.name))
        ancestry_rejection = reject_cli("Jolteon proof does not authenticate the withdrawal Header",
            args.binary, "bridge", "attest-payout", "--genesis", spec_path, "--l1-rpc", l1.url,
            "--request", bad_request, "--journal", args.output / "journal-0", "--key-file", fixture.guardian_keys[0])
    approvals = []
    for index, key in enumerate(fixture.guardian_keys):
        raw = run(args.binary, "bridge", "attest-payout", "--genesis", spec_path,
                  *finality_arguments, "--l1-rpc", l1.url, "--request", request_file, "--key-file", key,
                  "--journal", args.output / f"journal-{index}")
        approvals.append(json.loads(raw))
    votes = args.output / f"{name}-payout-approvals.json"
    write_json(votes, approvals[:3])
    settlement = json.loads(run(args.binary, "bridge", "settle", "--observations", votes))
    confirmed = comet_rpc.call("broadcast_tx_commit", tx=settlement)
    if int(confirmed["height"]) == 0 or confirmed["check_tx"]["code"] != 0:
        raise AssertionError(confirmed)
    settled = comet_rpc.call(
        "abci_query", path=json.dumps("/stem/bridge-withdrawal"), prove="true",
        data=hex_bytes(binary(lane) + le(nonce, 8)),
    )["response"]
    if settled["code"] != 0 or json.loads(base64.b64decode(settled["value"]))["record"] is not None:
        raise AssertionError(settled)
    return {"burn": burned, "l1_payout": payout, "l1_payout_proof": l1.prove_committed(payout),
            "settlement": confirmed, "signature_verification": verification, "capacity": capacity,
            "guardian_invalid_ancestry": ancestry_rejection,
            "direct_release": {"checkpoint": checkpoint, "pending_height": pending_height,
                                "proof_bytes": len(payload), "estimated_cycles": estimate, "rejections": negative,
                               "foreign_checkpoint": foreign_checkpoint,
                               "settled_pending_query": settled},
            "confirmed_height": approvals[0]["confirmed_height"],
            "confirmed_hash": approvals[0]["confirmed_hash"]}


def deploy_token(args, l1, fixture, vault):
    gate_bytes, token_bytes = args.xudt_gate.read_bytes(), args.xudt.read_bytes()
    gate_code, token_code = map(lambda data: hex_bytes(ckb_hash(data)), [gate_bytes, token_bytes])
    issuer = write_key(args.output, "xudt-issuer", secrets.token_bytes(32))
    issuer_lock = dict(fixture.standard, args=fixture.key(issuer)["blake160"])
    l1_type = script(token_code, hex_bytes(script_hash(issuer_lock)), "data1")
    funds = [(len(blob) + 200) * CKB for blob in (gate_bytes, token_bytes)]
    capital = 10_000 * CKB
    change = vault["change"] - sum(funds) - 3 * capital - FEE
    deploy = transaction([vault["change_point"]],
                         [output(amount, fixture.standard) for amount in funds]
                         + [output(capital, member) for member in (fixture.standard, issuer_lock, fixture.standard)]
                         + [output(change, fixture.standard)],
                         list(map(hex_bytes, (gate_bytes, token_bytes))) + ["0x"] * 4, [vault["secp"]])
    binaries = l1.send(fixture.sign("xudt-deploy", deploy, [wallet_group(0, [fixture.faucet])]))
    type_id = hex_bytes(ckb_hash(le(0, 8) + binary(binaries) + le(2, 4) + le(0, 8)))
    vault_type = dict(vault["type"], args=hex_bytes(binary(type_id) + binary(vault["type"]["args"])[32:]))
    vault_hash = hex_bytes(script_hash(vault_type))
    lock = dict(vault["lock"], args=hex_bytes(binary(vault_hash) + binary(vault["custody"]["args"])))
    escrow = script(gate_code, hex_bytes(binary(vault_hash) + b"\1"), "data1")
    prior = binary(vault["state"])
    initial = hex_bytes(bytes([2]) + prior[1:] + binary(gate_code) + script_hash(l1_type) + bytes(16))
    create = transaction([point(binaries, 2)],
                         [output(vault["reserve"], lock, vault_type),
                          output(capital - vault["reserve"] - FEE, fixture.standard)],
                         [initial, "0x"], [vault["vault_code"], vault["secp"]])
    receiver = l1.send(fixture.sign("xudt-vault", create, [wallet_group(0, [fixture.faucet])]))
    return dict(vault, vault=receiver, type=vault_type, vault_hash=vault_hash, state=initial, lock=lock,
                l1_type=l1_type, escrow_lock=escrow, gate_dep=dependency(point(binaries, 0)),
                token_dep=dependency(point(binaries, 1)), issuer=issuer,
                issuance=point(binaries, 3), fee_input=point(binaries, 4),
                fee_capacity=capital, escrow_capacity=600 * CKB)


def xudt_round_trip(args, l1, fixture, vault, spec, spec_path, baseline, prior, comet_rpc, relay):
    lane = hex_bytes(ckb_hash(b"Stem bridge lane\0" + binary(vault["genesis"]) + binary(vault["vault_hash"])
                              + b"\1" + script_hash(vault["l1_type"])))
    native = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())), fixture.key(fixture.owner)["blake160"], "data")
    gate_args = binary(vault["vault_hash"]) + b"\0" + binary(spec["network_id"]) + packed_script(native)
    gate = dict(vault["escrow_lock"], args=hex_bytes(gate_args))
    amount, capacity = 1200, 1200 * CKB
    issue = transaction([vault["issuance"]],
                        [output(capacity, gate, vault["l1_type"]),
                         output(vault["escrow_capacity"], vault["escrow_lock"], vault["l1_type"]),
                         output(vault["fee_capacity"] - capacity - vault["escrow_capacity"] - FEE, fixture.standard)],
                        [hex_bytes(le(amount, 16)), hex_bytes(bytes(16)), "0x"], [vault["token_dep"], vault["secp"]])
    deposit = l1.send(fixture.sign("xudt-issue", issue, [wallet_group(0, [vault["issuer"]])]))
    consolidation = transaction([point(vault["vault"], 0), point(deposit, 0), point(deposit, 1), vault["fee_input"], vault["custody_cell"]],
                                [output(vault["reserve"] + capacity, vault["lock"], vault["type"]),
                                 output(vault["escrow_capacity"], vault["escrow_lock"], vault["l1_type"]),
                                 output(vault["fee_capacity"] - FEE, fixture.standard),
                                 output(vault["authorization"], vault["custody"])],
                                [vault["state"], hex_bytes(le(amount, 16)), "0x", "0x"],
                                [vault["vault_code"], vault["gate_dep"], vault["token_dep"], vault["multisig"], vault["secp"]])
    groups = [wallet_group(4, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)), wallet_group(3, [fixture.faucet])]
    consolidated = l1.send(fixture.sign("xudt-consolidate", consolidation, groups))
    vault["custody_cell"] = point(consolidated, 3)
    stem_dep = point(baseline["mint_id"], 2)
    encoded, minted, votes, rejected = attain_mint(args, l1, fixture, vault, spec_path, lane, consolidated, comet_rpc,
                                                   "xudt", prior["confirmed_height"], prior["confirmed_hash"],
                                                   [dependency(stem_dep)])
    tx_id = stem_id(args, spec["network_id"], encoded)
    mint = {"l1_consolidation": consolidated, "l1_proof": l1.prove_committed(consolidated), "mint": minted,
            "mint_id": tx_id, "capacity": capacity, "payment_capacity": capacity,
            "payment": point(tx_id, 0), "lock_dep": baseline["lock_dep"], "minority_rejected": rejected,
            "confirmed_height": votes[0]["mint"]["confirmed_height"], "confirmed_hash": votes[0]["mint"]["confirmed_hash"]}
    wrapped = script(spec["bridge"]["wrapped_code_hash"], hex_bytes(script_hash(vault["l1_type"])), "data")
    payout = withdraw_and_settle(args, l1, fixture, vault, spec, spec_path, lane, mint, comet_rpc, relay,
                                 typed={"wrapped": wrapped, "amount": amount, "stem_dep": stem_dep})
    return {"mint": mint, "withdrawal": payout, "units": amount, "lane": lane}


def vault_operations(args, l1, fixture, native, token, ckb_lane, ckb, typed, spec_path, comet):
    amount = FEE // 2
    source = point(ckb["l1_payout"], 1)
    destination = point(typed["withdrawal"]["l1_payout"], 1)
    fee_input = point(typed["withdrawal"]["l1_payout"], 3)
    inputs = [source, destination, fee_input, token["custody_cell"]]
    cell = lambda out: l1.call("get_live_cell", out, True)["cell"]["output"]
    before, after, change, custody_coin = map(cell, inputs)
    cap = lambda out: int(out["capacity"], 16)
    native_config, typed_config = [binary(vault["state"]) for vault in (native, token)]

    def state(config, action, transfer=0, peer=bytes(32)):
        payload = binary(vault_state(action, 1, hex_bytes(config[10:42]), hex_bytes(config[42:74]),
                                     amount=transfer, payout_hash=peer))
        return hex_bytes(bytes([config[0]]) + payload[1:] + config[146:])

    def observe(transition, source, prior, destination=None):
        request = args.output / "vault-observation.json"
        write_json(request, {"tx": transition, "source": source, "destination": destination,
                             "previous_l1_height": prior["confirmed_height"], "previous_l1_hash": prior["confirmed_hash"]})
        l1.mine(2)
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            signed = run(args.binary, "bridge", "attest-vault", "--genesis", spec_path,
                         "--l1-rpc", l1.url, "--request", request, "--key-file", key,
                         "--journal", args.output / f"journal-{index}")
            votes.append(json.loads(signed))
        observations = args.output / "vault-approvals.json"
        write_json(observations, votes)
        tx = json.loads(run(args.binary, "bridge", "settle", "--observations", observations))
        result = comet.call("broadcast_tx_commit", tx=tx)
        if result["check_tx"]["code"] != 0 or int(result["height"]) == 0:
            raise AssertionError(result)
        return {"settlement": result, "confirmed_height": votes[0]["confirmed_height"],
                "confirmed_hash": votes[0]["confirmed_hash"], "l1_transaction": transition,
                "l1_proof": l1.prove_committed(transition)}

    groups = [wallet_group(3, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
              wallet_group(2, [fixture.faucet])]
    outs = [output(cap(before) - amount, before["lock"], before["type"]),
            output(cap(after) + amount, after["lock"], after["type"]),
            output(cap(change) - FEE, fixture.standard), output(cap(custody_coin), native["custody"])]
    data = [state(native_config, 3, amount, binary(token["vault_hash"])),
            state(typed_config, 4, amount, binary(native["vault_hash"])), "0x", "0x"]
    deps = [native["vault_code"], native["multisig"], native["secp"]]
    wrong = [*outs]
    wrong[1] = output(cap(after) + amount - 1, after["lock"], after["type"])
    invalid = l1.reject(fixture.sign("invalid-reserve", transaction(inputs, wrong, data, deps), groups))
    missing_custody = l1.reject(fixture.sign("reserve-without-custody",
                                           transaction(inputs[:-1], outs[:-1], data[:-1], deps),
                                           [wallet_group(2, [fixture.faucet])]))
    moved = l1.send(fixture.sign("move-reserve", transaction(inputs, outs, data, deps), groups))
    transfer = observe(moved, {"lane": ckb_lane, "vault": source}, typed["withdrawal"],
                       {"lane": typed["lane"], "vault": destination})
    transfer.update(capacity=amount, mismatch_rejected=invalid, missing_custody_rejected=missing_custody)

    incoming = [write_key(args.output, f"incoming-custody-{index}", secrets.token_bytes(32)) for index in range(3)]
    script_bytes = bytes([0, 0, 2, 3]) + b"".join(binary(fixture.key(key)["blake160"]) for key in incoming)
    next_authority = ckb_hash(script_bytes)[:20]
    lock = dict(before["lock"], args=hex_bytes(binary(native["vault_hash"]) + next_authority))
    custody_lock = dict(native["custody"], args=hex_bytes(next_authority))
    fee_cap = cap(cell(point(moved, 2)))
    rotate = transaction([point(moved, 0), point(moved, 2), point(moved, 3)],
                         [output(cap(before) - amount, lock, before["type"]), output(fee_cap - FEE, fixture.standard),
                          output(cap(custody_coin), custody_lock)],
                         [state(native_config, 2), "0x", "0x"], deps)
    old_groups = [wallet_group(2, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)), wallet_group(1, [fixture.faucet])]
    rotation = l1.send(fixture.sign("rotate-custody", rotate, old_groups))
    rotated = observe(rotation, {"lane": ckb_lane, "vault": point(moved, 0)}, transfer)
    if cell(point(rotation, 0))["lock"] != lock:
        raise AssertionError("CKB rotated vault does not use the incoming custody lock")
    height = rotated["confirmed_height"]
    l1.call("truncate", l1.call("get_block_hash", hex(height - 1)))
    l1.mine(3)
    replacement = l1.call("get_block_hash", hex(height))
    if replacement == rotated["confirmed_hash"]:
        raise AssertionError("CKB test node reproduced the vault observation anchor")
    error = reject_cli(
        "previous L1 anchor was reorganized",
        args.binary, "bridge", "attest-vault", "--genesis", spec_path, "--l1-rpc", l1.url,
        "--request", args.output / "vault-observation.json", "--key-file", fixture.guardian_keys[0],
        "--journal", args.output / "journal-0",
    )
    return {"reserve_transfer": transfer, "vault_rotation": rotated,
            "reorganized_anchor_rejected": error, "replacement_l1_hash": replacement}



def reconcile_backing(comet_rpc, report):
    lanes = [report]
    if "xudt" in report:
        lanes.append(report["xudt"])
    minted = sum(lane["mint"]["capacity"] for lane in lanes)
    paid = sum(lane["withdrawal"]["capacity"] for lane in lanes)
    expected = {
        "liquid_shannons": minted - paid,
        "circulating_shannons": minted - sum(lane["mint"]["payment_capacity"] for lane in lanes),
        "pending_shannons": 0,
        "unallocated_shannons": FEE * len(lanes),
    }
    response = comet_rpc.call("abci_query", path=json.dumps("/stem/bridge-backing"),
                              prove="true")["response"]
    if response["code"] != 0:
        raise AssertionError(response)
    backing = json.loads(base64.b64decode(response["value"]))
    measured = {key: int(backing[key], 16) for key in expected}
    if measured != expected or any(not binary(backing[key]) for key in ("record", "proof")):
        raise AssertionError((measured, expected))
    return {"height": response["height"], **backing}


def lane_proof(comet_rpc, lane):
    response = comet_rpc.call("abci_query", path=json.dumps("/stem/bridge-lane"),
                              data=lane, prove="true")["response"]
    if response["code"] != 0:
        raise AssertionError(response)
    result = json.loads(base64.b64decode(response["value"]))
    if any(not binary(result[key]) for key in ("record", "proof")):
        raise AssertionError(result)
    return {"height": response["height"], **result}


def verify_l1_output(args, l1, payout, vault_spent):
    txid = payout["l1_payout"]
    bundle = payout["l1_payout_proof"]
    packed = l1.call("get_transaction", txid, "0x0", True)["transaction"]
    transaction_path = args.output / "ckb-payout-packed.json"
    proof_path = args.output / "ckb-payout-inclusion.json"
    write_json(transaction_path, packed)
    write_json(proof_path, bundle)
    verify = [
        args.binary, "ckb-wallet", "verify-inclusion",
        "--block-hash", bundle["header"]["hash"], "--tx-hash", txid,
    ]
    command = [*verify, "--transaction", transaction_path]
    result = json.loads(run(*command, "--proof", proof_path, "--output-index", "0"))
    if int(result["output"]["capacity"], 16) != payout["capacity"]:
        raise AssertionError("authenticated CKB output differs from the payout receipt")
    altered = json.loads(json.dumps(bundle))
    indices = altered["proof"]["witnesses_proof"]["indices"]
    indices[0] = hex(int(indices[0], 16) ^ 1)
    altered_path = args.output / "ckb-payout-inclusion-altered.json"
    write_json(altered_path, altered)
    reject_cli("matching transaction and witness position", *command,
               "--proof", altered_path, "--output-index", "0")
    changed_witness = bytearray(binary(packed))
    changed_witness[-1] ^= 1
    changed_path = args.output / "ckb-payout-witness-altered.json"
    write_json(changed_path, hex_bytes(changed_witness))
    reject_cli("does not match the Header", *verify, "--transaction", changed_path, "--proof", proof_path,
               "--output-index", "0")
    evidence = {"payout": result}
    if vault_spent:
        old_vault = json.loads(run(*command, "--proof", proof_path, "--output-index", "1"))
        status = l1.call("get_live_cell", old_vault["out_point"], True)["status"]
        if status == "live":
            raise AssertionError("reserve transfer did not consume the old CKB vault")
        evidence.update(historical_vault=old_vault, historical_vault_current_status=status)
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "comet", "vault", "deposit-lock", "native-lock", "wrapped", "proof-verifier", "direct-checkpoint", "output"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True)
    for name in ("xudt", "xudt-gate"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    parser.add_argument("--l1-port", type=int, default=43110)
    parser.add_argument("--comet-port", type=int, default=43120)
    args = parser.parse_args()
    if bool(args.xudt) != bool(args.xudt_gate):
        parser.error("--xudt and --xudt-gate must be specified together")
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    children = []
    comet_rpc = Rpc(args.comet_port)
    try:
        fixture = Fixtures(args)
        network = hex_bytes(ckb_hash(b"Stem CKB bridge fixture\0" + secrets.token_bytes(32)))
        vault = deploy(args, l1, fixture, network)
        token = deploy_token(args, l1, fixture, vault) if args.xudt else None
        comet, spec, spec_path, lane = native_spec(args, l1, fixture, vault, token)
        home = args.output / "stem"
        children.append(start(args, [args.binary, comet_app_command(args.binary), "--home", home, "--genesis", spec_path, "--archive"], "stem.log"))
        wait_for(lambda: (home / "abci.sock").is_socket(), "Stem ABCI socket")
        children.append(start(args, [args.comet, "start", "--home", comet,
            "--proxy_app", f"unix://{home / 'abci.sock'}", "--rpc.laddr", f"tcp://127.0.0.1:{args.comet_port}",
            "--p2p.laddr", f"tcp://127.0.0.1:{args.comet_port + 1}"], "comet.log"))
        wait_for(lambda: int(comet_rpc.call("status")["sync_info"]["latest_block_height"]) > 0, "CometBFT height")
        relay = Relay(args, l1, fixture, vault["anchor"], spec_path, comet / "config/genesis.json", comet_rpc)
        relay.advance(0, vault["secp"])
        result = deposit_and_mint(args, l1, fixture, vault, spec, spec_path, lane, comet_rpc)
        write_json(args.output / "native-mint-report.json", result)
        payout = withdraw_and_settle(args, l1, fixture, vault, spec, spec_path, lane, result, comet_rpc, relay)
        report = {"mint": result, "withdrawal": payout, "checkpoint_type": relay.type, "checkpoints": relay.publications}
        if token:
            token["custody_cell"] = vault["custody_cell"]
            report["xudt"] = xudt_round_trip(args, l1, fixture, token, spec, spec_path, result, payout, comet_rpc, relay)
            report["vault_operations"] = vault_operations(args, l1, fixture, vault, token, lane, payout,
                                                             report["xudt"], spec_path, comet_rpc)
            print(f"CKB xUDT payout {report['xudt']['withdrawal']['l1_payout']} for {report['xudt']['units']} units")
        report["backing"] = reconcile_backing(comet_rpc, report)
        report["authenticated_ckb_outputs"] = verify_l1_output(args, l1, payout, token is not None)
        report["lane_proofs"] = {lane: lane_proof(comet_rpc, lane)}
        if token:
            asset_lane = report["xudt"]["lane"]
            report["lane_proofs"][asset_lane] = lane_proof(comet_rpc, asset_lane)
        write_json(args.output / "native-bridge-report.json", report)
        print(f"CKB payout {payout['l1_payout']} for {payout['capacity']} shannons, native settlement height {payout['settlement']['height']}")
        print(f"CKB consolidation {result['l1_consolidation']}, native Stem mint {result['mint_id']}, height {result['mint']['height']}")
    finally:
        comet_rpc.close()
        for child in reversed(children):
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        l1.stop()


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Run the configured xUDT lane on an unmodified CKB L1 and four sidechain replicas."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import secrets
import time

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, SIGHASH_CODE, Fixtures, L1, binary, ckb_hash,
    dependency, hex_bytes, le, output, payout_command, point, prepare_payout, query_withdrawal, reject_cli, rotate_vault, script, script_hash,
    sidechain_tx, transaction, vault_state, wait_sidechain_tx, wallet_group, write_key,
)
from localnet import Rpc, await_genesis, config_value, execution_proof, export_execution_proof, init_network, run, set_sync_config, write_json
from governance_localnet import wait_height
from snapshot_localnet import query_tx


def units(amount):
    return hex_bytes(le(amount, 16))


def verify_rotated_xudt(args, l1, f, node, app, first_release, first_burn, journals, l1_deps, side_deps, beneficiary):
    change = first_burn["json"]["outputs"][1]
    amount = int.from_bytes(binary(first_burn["json"]["outputs_data"][1]), "little")
    capacity = int(change["capacity"], 16) - FEE
    memo = descriptor(args, "burn-cell", app, beneficiary, amount)
    burn = transaction([point(first_burn["hash"], 1)], [output(capacity, memo["lock"], memo["type"])],
                       [memo["data"]], side_deps)
    signed = f.sign("pending-rotation-burn", burn, [wallet_group(0, [f.friend])])
    receipt = sidechain_tx(node, signed["molecule"])
    pending = query_withdrawal(node, 1)
    if (pending["out_point"], pending["units"], pending["capacity"]) != (point(signed["hash"], 0), str(amount), capacity):
        raise RuntimeError("Second typed burn was not assigned to withdrawal slot one")
    proof = args.output / "rotated-withdrawal-proof.json"
    export_execution_proof(node, int(receipt["height"]), signed["hash"], proof)
    escrow = point(first_release, 2)
    before = l1.call("get_live_cell", escrow, True)
    rotated, keys, prefix, report = rotate_vault(args, l1, f, app, point(first_release, 1), point(first_release, 3), l1_deps)
    if report["checked_rotation"]["next_withdrawal_nonce"] != 1:
        raise RuntimeError("Typed rotation changed the pending withdrawal slot")
    if l1.call("get_live_cell", escrow, True) != before or before["status"] != "live":
        raise RuntimeError("Custody migration touched the funded xUDT escrow")
    inputs = [point(rotated, 0), escrow, point(rotated, 1)]
    candidate = prepare_payout(args, l1, node, proof, journals, inputs, l1_deps, typed=True)
    path = args.output / "rotated-release-candidate.json"
    write_json(path, candidate)
    checked = json.loads(run(*payout_command(args, l1, node, proof, journals, inputs[0], path)))
    rejected = l1.reject(f.sign("old-custody-payout", candidate, signers(f, 2)))
    if "Inputs[0].Lock" not in rejected:
        raise RuntimeError(f"The rotated custody lock did not reject the old signers: {rejected}")
    release = f.sign("rotated-release", candidate, [wallet_group(0, keys[:2], hex_bytes(prefix)),
                                                    wallet_group(2, [f.faucet])])
    if release["hash"] != checked["l1_transaction_hash"]:
        raise RuntimeError("Migrated custody signed a different typed payout")
    released = l1.send(release)
    for index, expected in enumerate(candidate["outputs"][:3]):
        live = l1.call("get_live_cell", point(released, index), True)
        if (live["status"], live["cell"]["output"], live["cell"]["data"]["content"]) != (
                "live", expected, candidate["outputs_data"][index]):
            raise RuntimeError(f"Unexpected migrated xUDT payout output {index}")
    nonce = int.from_bytes(binary(candidate["outputs_data"][1])[2:10], "little")
    if (nonce, candidate["outputs"][2], candidate["outputs_data"][2]) != (2, before["cell"]["output"], units(0)):
        raise RuntimeError("Migrated payout did not preserve the escrow carrier or advance the nonce")
    return dict(report, l1_release=released, withdrawal_slot=1, new_vault_slot=2,
                pending_burn_proved_before_rotation=True, escrow_unchanged_during_rotation=True,
                prior_signers_rejected=rejected, released_units=str(amount), escrow_remaining_units="0",
                released_shannons=capacity, l1_release_proof=l1.prove_committed(released))


def state(action, nonce, deposit_code, multisig, gate, asset, amount=0,
          recipient=bytes(32), withdrawal=bytes(32), quantity=0):
    native = binary(vault_state(action, nonce, deposit_code, multisig, amount, recipient, withdrawal))
    return hex_bytes(bytes([2]) + native[1:] + binary(gate) + asset + le(quantity, 16))


def descriptor(args, command, genesis, recipient=None, amount=None):
    options = []
    if recipient is not None:
        flag = "--l1-recipient" if command == "burn-cell" else "--recipient"
        options += [flag, recipient]
    if amount is not None:
        options += ["--units", str(amount)]
    return json.loads(run(args.binary, "bridge", command, "--genesis", genesis, *options))


def signers(fixture, fee_index):
    return [wallet_group(0, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
            wallet_group(fee_index, [fixture.faucet])]


def reject_script(l1, signed, source, code_hash, error_code):
    failure = l1.reject(signed)
    error_url = f"/by-data-hash/{code_hash[2:]}.html#{error_code} "
    if f"source: {source}, cause: ValidationFailure:" not in failure or error_url not in failure:
        raise RuntimeError(f"Expected {source} {code_hash} error {error_code}, got {failure}")
    return failure


def attest_mint(args, l1, fixture, nodes, genesis, deposit, consolidation):
    votes = []
    for index, key in enumerate(fixture.guardian_keys):
        vote = json.loads(run(
            args.binary, "bridge", "attest", "--genesis", genesis,
            "--l1-rpc", l1.url, "--deposit-tx", deposit, "--output-index", "0",
            "--consolidation-tx", consolidation, "--key-file", key,
            "--journal", args.output / f"guardian-journal-{index}",
        ))
        path = args.output / f"vote-{index}.json"
        write_json(path, vote)
        votes.append(path)
    submit = [args.binary, "bridge", "mint", "--genesis", genesis,
              "--submit-rpc", f"http://127.0.0.1:{nodes[0].rpc}/"]
    mint = json.loads(run(*submit, *votes[:3]))
    if mint["execution"]["already_finalized"]:
        raise RuntimeError("A fresh xUDT mint unexpectedly existed")
    rpc = Rpc(nodes[0].rpc)
    try:
        verified = wait_sidechain_tx(rpc, hashlib.sha256(binary(mint["transaction"])).hexdigest())
    finally:
        rpc.close()
    retry = json.loads(run(*submit, *votes))
    if retry["ckb_tx_hash"] != mint["ckb_tx_hash"] or retry["transaction"] == mint["transaction"]:
        raise RuntimeError("Alternate attestation must preserve the deposit's raw identity")
    if retry["execution"] != {**mint["execution"], "already_finalized": True}:
        raise RuntimeError("An alternate attestation must return the existing mint")
    duplicate = sidechain_tx(nodes[0], retry["transaction"], required=False)
    if "transaction already exists" not in duplicate.get("log", ""):
        raise RuntimeError(f"Unexpected duplicate mint rejection: {duplicate}")
    return mint["ckb_tx_hash"], verified["height"], duplicate["code"]


def verify_compact_bridge(args, nodes, app_state, minted, burn_height, payout, ordinary):
    joining = nodes[-1]
    joining.compact_transaction_history = True
    checkpoint = (burn_height // args.checkpoint_interval + 1) * args.checkpoint_interval
    rpc, joined = Rpc(nodes[0].rpc), Rpc(joining.rpc)
    try:
        wait_height(rpc, checkpoint + 2)
        trust_hash = rpc.call("block", height=checkpoint)["block_id"]["hash"]
        set_sync_config(joining, nodes[:2], checkpoint, trust_hash)
        started = time.monotonic()
        joining.start()
        await_genesis([joining])
        logs = (joining.directory / "app.log").read_text()
        match = re.search(r"event=snapshot_installed height=(\d+)", logs)
        if not match or int(match[1]) < checkpoint:
            raise RuntimeError("Replica did not restore the checkpoint containing the bridge history")
        if query_withdrawal(joining, 0) != payout or query_tx(joined, ordinary[2:]) is not None:
            raise RuntimeError("Restored transaction history differs from the bridge policy")
        votes = sorted(args.output.glob("vote-*.json"))
        retry = json.loads(run(args.binary, "bridge", "mint", "--genesis", app_state,
                               "--submit-rpc", f"http://127.0.0.1:{joining.rpc}/", *votes))
        if retry["ckb_tx_hash"] != minted or not retry["execution"]["already_finalized"]:
            raise RuntimeError("Compact restore did not preserve the original bridge mint")
        rejected = joined.broadcast_packed(binary(retry["transaction"]))
        if not rejected["code"] or rejected["log"] != "transaction already exists":
            raise RuntimeError(f"Restored replica accepted a duplicate mint: {rejected}")
        return {"snapshot_height": int(match[1]), "seconds": time.monotonic() - started,
                "mint_retry": retry["execution"], "rejects_duplicate_mint": rejected["log"],
                "withdrawal_record_preserved": True, "ordinary_transaction_body_omitted": True}
    finally:
        rpc.close()
        joined.close()


def scenario(args, l1, f):
    programs = [args.vault, args.deposit_lock, args.xudt_gate, args.xudt]
    code = [path.read_bytes() for path in programs]
    vault_code, deposit_code, gate_code, token_code = [hex_bytes(ckb_hash(blob)) for blob in code]
    code_capacity = [(len(blob) + 200) * CKB for blob in code]
    genesis = l1.call("get_block_by_number", "0x0")
    cellbase, dep_groups = genesis["transactions"][:2]
    faucet_index, funds = next((i, cell) for i, cell in enumerate(cellbase["outputs"])
                               if cell["lock"]["args"] == FAUCET_ARG)
    multisig_code = hex_bytes(script_hash(cellbase["outputs"][4]["type"]))
    multisig_lock = script(multisig_code, hex_bytes(ckb_hash(f.multisig_prefix)[:20]))
    issuer_key = write_key(args.output, "xudt-issuer", secrets.token_bytes(32))
    issuer = script(SIGHASH_CODE, f.key(issuer_key)["blake160"])
    l1_type = script(token_code, hex_bytes(script_hash(issuer)), "data1")
    token_hash = script_hash(l1_type)
    l1_secp = dependency(point(dep_groups["hash"], 0), "dep_group")
    l1_multisig = dependency(point(dep_groups["hash"], 1), "dep_group")
    fee_cap = 12_000 * CKB
    change = int(funds["capacity"], 16) - sum(code_capacity) - 4 * fee_cap - FEE
    deploy = transaction(
        [point(cellbase["hash"], faucet_index)],
        [output(cap, f.standard) for cap in code_capacity]
        + [output(fee_cap, lock) for lock in (f.standard, issuer, f.standard, f.standard)]
        + [output(change, f.standard)],
        [hex_bytes(blob) for blob in code] + ["0x"] * 5,
        [l1_secp],
    )
    deployment = l1.send(f.sign("deploy", deploy, [wallet_group(0, [f.faucet])]))
    vault_dep, gate_dep, xudt_dep = [dependency(point(deployment, i)) for i in (0, 2, 3)]
    vault_args = hex_bytes(ckb_hash(le(0, 8) + binary(deployment) + le(4, 4) + le(0, 8)))
    vault_type = script(vault_code, vault_args, "data1")
    vault_hash = hex_bytes(script_hash(vault_type))
    reserve = 1_600 * CKB
    initial = state(0, 0, deposit_code, multisig_code, gate_code, token_hash)
    create = transaction(
        [point(deployment, 4)],
        [output(reserve, multisig_lock, vault_type), output(fee_cap - reserve - FEE, f.standard)],
        [initial, "0x"],
        [vault_dep, l1_secp],
    )
    vault = l1.send(f.sign("vault", create, [wallet_group(0, [f.faucet])]))
    bridge = {
        "l1_genesis": genesis["header"]["hash"], "vault_type_hash": vault_hash,
        "deposit_lock": script(deposit_code, vault_hash, "data1"),
        "xudt": {"gate_code_hash": gate_code, "l1_type": l1_type},
        "confirmations": 2, "guardians": f.guardians,
    }
    nodes, side = init_network(args, bridge=bridge, observers=int(args.checkpoint_compact_transactions))
    validators = nodes[:args.replicas]
    if args.checkpoint_live_cell_replay:
        config_value(nodes[2].config, "indexer", '"null"')
    try:
        for node in validators:
            node.start()
        await_genesis(validators)
        app = args.output / "app-state.json"
        owner = args.output / "owner.json"
        beneficiary = args.output / "beneficiary.json"
        write_json(owner, f.owner_lock)
        write_json(beneficiary, f.friend_lock)
        escrow = descriptor(args, "escrow-cell", app)
        token_holder = 1_200 * CKB
        escrow_cap = 600 * CKB
        issued_units = 1200
        issue = transaction(
            [point(deployment, 5)],
            [output(token_holder, f.owner_lock, l1_type),
             output(escrow_cap, escrow["lock"], escrow["type"]),
             output(fee_cap - token_holder - escrow_cap - FEE, f.standard)],
            [units(issued_units), escrow["data"], "0x"],
            [xudt_dep, l1_secp],
        )
        issuance = l1.send(f.sign("issue", issue, [wallet_group(0, [issuer_key])]))
        memo = descriptor(args, "deposit-cell", app, owner, issued_units)
        custody_cap = token_holder - FEE
        deposit = transaction(
            [point(issuance, 0)], [output(custody_cap, memo["lock"], memo["type"])],
            [memo["data"]], [xudt_dep, l1_secp],
        )
        deposit_hash = l1.send(f.sign("deposit", deposit, [wallet_group(0, [f.owner])]))
        unauthorized = transaction(
            [point(deposit_hash, 0)], [output(custody_cap - FEE, f.standard, l1_type)],
            [units(issued_units)], [gate_dep, xudt_dep],
        )
        unauthorized_error = reject_script(
            l1, f.sign("unauthorized", unauthorized, []), "Inputs[0].Lock", gate_code, 10)
        vault_cap = reserve + custody_cap
        consolidation_inputs = [point(vault, 0), point(deposit_hash, 0), point(issuance, 1), point(deployment, 6)]
        deps = [vault_dep, gate_dep, xudt_dep, l1_multisig, l1_secp]
        diversion_cap = 400 * CKB
        diversion = transaction(
            consolidation_inputs,
            [output(vault_cap, multisig_lock, vault_type),
             output(escrow_cap, escrow["lock"], l1_type),
             output(diversion_cap, f.standard, l1_type),
             output(fee_cap - diversion_cap - FEE, f.standard)],
            [initial, units(issued_units - 1), units(1), "0x"], deps,
        )
        diversion_error = reject_script(
            l1, f.sign("divert-deposit", diversion, signers(f, 3)), "Inputs[0].Type", vault_code, 14)
        consolidation = transaction(
            consolidation_inputs,
            [output(vault_cap, multisig_lock, vault_type), output(escrow_cap, escrow["lock"], l1_type),
             output(fee_cap - FEE, f.standard)],
            [initial, units(issued_units), "0x"], deps,
        )
        consolidation_hash = l1.send(f.sign("consolidate", consolidation, signers(f, 3)))
        l1.mine(bridge["confirmations"])
        minted, mint_height, duplicate_code = attest_mint(args, l1, f, nodes, app, deposit_hash, consolidation_hash)
        released_units = 500
        burn_info = descriptor(args, "burn-cell", app, beneficiary, released_units)
        side_type = burn_info["type"]
        if side_type == l1_type:
            raise RuntimeError("Wrapped asset must have an independent owner identity")
        side_deps = [dependency(point(side["deposit_dep_group"], 0), "dep_group"),
                     dependency(side["xudt_dep"])]
        pay = transaction(
            [point(minted, 0)], [output(custody_cap - FEE, f.friend_lock, side_type)],
            [units(issued_units + 1)], side_deps,
        )
        bad_pay = f.sign("inflate", pay, [wallet_group(0, [f.owner])])
        inflation = sidechain_tx(nodes[0], bad_pay["molecule"], required=False)
        if "wrapped xUDT must conserve units" not in inflation.get("log", ""):
            raise RuntimeError(f"Unexpected wrapped supply rejection: {inflation}")
        pay["outputs_data"] = [units(issued_units)]
        paid = f.sign("pay", pay, [wallet_group(0, [f.owner])])
        payment = sidechain_tx(nodes[0], paid["molecule"])
        claim = 850 * CKB
        retained_units = issued_units - released_units
        burn = transaction(
            [point(paid["hash"], 0)],
            [output(claim, burn_info["lock"], side_type),
             output(custody_cap - 2 * FEE - claim, f.friend_lock, side_type)],
            [burn_info["data"], units(retained_units)], side_deps,
        )
        burned = f.sign("burn", burn, [wallet_group(0, [f.friend])])
        burn_receipt = sidechain_tx(nodes[0], burned["molecule"])
        payout = query_withdrawal(nodes[0], 0)
        if (payout["capacity"], payout["units"], payout["l1_type"], payout["out_point"]) != (
                claim, str(released_units), l1_type, point(burned["hash"], 0)):
            raise RuntimeError(f"Typed withdrawal differs from the executed burn: {payout}")
        if args.checkpoint_live_cell_replay:
            export_execution_proof(nodes[2], int(burn_receipt["height"]), burned["hash"],
                                   args.output / "execution-proof.json")
        else:
            execution_proof(nodes[0], {burn_receipt["hash"].lower(): {"height": int(burn_receipt["height"])}}, args.output)
        verification = run(args.proof_verifier, "-proof", args.output / "execution-proof.json",
                           "-genesis", nodes[0].home / "config" / "genesis.json")
        for index in range(len(f.guardian_keys)):
            run(args.binary, "bridge", "check-anchor", "--genesis", app,
                "--l1-rpc", l1.url, "--journal", args.output / f"guardian-journal-{index}")

        withdrawal_id = binary(payout["withdrawal_id"])
        recipient_hash = script_hash(payout["l1_lock"])
        release_state = state(1, 1, deposit_code, multisig_code, gate_code, token_hash,
                              claim, recipient_hash, withdrawal_id, released_units)
        release_inputs = [point(consolidation_hash, 0), point(consolidation_hash, 1), point(deployment, 7)]
        common = [output(vault_cap - claim, multisig_lock, vault_type), output(escrow_cap, escrow["lock"], l1_type)]
        wrong_payout = transaction(
            release_inputs,
            [output(claim, payout["l1_lock"]), *common,
             output(diversion_cap, f.standard, l1_type), output(fee_cap - diversion_cap - FEE, f.standard)],
            ["0x", release_state, units(retained_units), units(released_units), "0x"], deps,
        )
        wrong_error = reject_script(
            l1, f.sign("wrong-payout-type", wrong_payout, signers(f, 2)), "Inputs[0].Type", vault_code, 15)
        amount_shift = transaction(
            release_inputs,
            [output(claim, payout["l1_lock"], l1_type), *common,
             output(diversion_cap, f.standard, l1_type), output(fee_cap - diversion_cap - FEE, f.standard)],
            [units(released_units - 1), release_state, units(retained_units), units(1), "0x"], deps,
        )
        amount_error = reject_script(
            l1, f.sign("wrong-payout-units", amount_shift, signers(f, 2)), "Inputs[0].Type", vault_code, 15)
        expected = transaction(
            release_inputs,
            [output(claim, payout["l1_lock"], l1_type), *common, output(fee_cap - FEE, f.standard)],
            [units(released_units), release_state, units(retained_units), "0x"], deps,
        )
        candidate = args.output / "release.json"
        proof_file = args.output / "execution-proof.json"
        journals = [args.output / f"guardian-journal-{index}" for index in range(len(f.guardian_keys))]
        release = prepare_payout(args, l1, nodes[0], proof_file, journals, release_inputs, deps, typed=True)
        if release != expected:
            raise RuntimeError("Prepared payout differs from the expected typed release")
        command = payout_command(args, l1, nodes[0], proof_file, journals, release_inputs[0], candidate)
        for invalid in (wrong_payout, amount_shift):
            write_json(candidate, invalid)
            reject_cli("L1 recipient, capacity, asset or amount differs", *command)
        write_json(candidate, release)
        authorization = json.loads(run(*command))
        signed_release = f.sign("release", release, signers(f, 2))
        if authorization["l1_transaction_hash"] != signed_release["hash"]:
            raise RuntimeError("Typed payout signature must bind the checked transaction")
        released = l1.send(signed_release)
        for index, expected in enumerate(release["outputs"][:3]):
            live = l1.call("get_live_cell", point(released, index), True)
            if (live["status"], live["cell"]["output"], live["cell"]["data"]["content"]) != (
                    "live", expected, release["outputs_data"][index]):
                raise RuntimeError(f"L1 output {index} differs from the authorized release")
        replay = transaction(
            [point(released, 1), point(released, 2), point(released, 3)],
            [output(claim, payout["l1_lock"], l1_type),
             output(vault_cap - 2 * claim, multisig_lock, vault_type), common[1],
             output(fee_cap - 2 * FEE, f.standard)],
            [units(released_units), state(1, 2, deposit_code, multisig_code, gate_code, token_hash,
                                         claim, recipient_hash, withdrawal_id, released_units),
             units(retained_units - released_units), "0x"], deps,
        )
        replay_error = reject_script(
            l1, f.sign("replayed-payout", replay, signers(f, 2)), "Inputs[0].Type", vault_code, 14)
        with args.binary.open("rb") as executable:
            sidechain_sha256 = hashlib.file_digest(executable, "sha256").hexdigest()
        report = {
            "l1_version": run(args.l1, "--version"), "l1_genesis": bridge["l1_genesis"],
            "vault": vault, "deposit": deposit_hash, "consolidation": consolidation_hash,
            "l1_type": l1_type, "sidechain_type": side_type,
            "xudt_elf_sha256": hashlib.sha256(code[3]).hexdigest(),
            "artifacts": {
                "sidechain_sha256": sidechain_sha256,
                "cometbft_version": run(args.comet, "version"),
                "vault_type_code_hash": vault_code,
                "native_deposit_code_hash": deposit_code,
                "xudt_gate_code_hash": gate_code,
                "replicas": args.replicas, "vm_workers": args.vm_workers, "commit_ms": args.commit_ms,
                "l1_confirmations": bridge["confirmations"],
            },
            "minted_units": str(issued_units), "minted_capacity": custody_cap, "mint_height": mint_height,
            "payment_height": payment["height"], "burn_height": burn_receipt["height"],
            "released_units": str(released_units), "released_capacity": claim,
            "escrow_remaining_units": str(retained_units), "escrow_capacity": escrow_cap,
            "withdrawal": payout, "l1_release": released, "proof_verification": verification,
            "payout_authorization": authorization, "payout_prepared_by_cli": True,
            "rejects": {"without_vault": unauthorized_error, "diversion": diversion_error,
                        "sidechain_inflation": inflation["code"], "duplicate_mint": duplicate_code,
                        "wrong_payout_type": wrong_error, "wrong_payout_units": amount_error,
                        "payout_replay": replay_error},
        }
        if args.checkpoint_compact_transactions:
            report["compact_bridge_restore"] = verify_compact_bridge(
                args, nodes, app, minted, int(burn_receipt["height"]), payout, paid["hash"])
        if args.checkpoint_live_cell_replay:
            if "tx_index.db" in nodes[2].storage():
                raise RuntimeError("The withdrawal proof provider created a CometBFT transaction index")
            report["index_free_withdrawal_proof"] = True
        report["custody_migration"] = verify_rotated_xudt(args, l1, f, nodes[0], app, released, burned,
                                                          journals, deps, side_deps, beneficiary)
        write_json(args.output / "xudt-round-trip.json", report)
        print(json.dumps(report, indent=2))
    finally:
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "l1", "vault", "deposit-lock", "xudt-gate", "xudt", "proof-verifier", "output"):
        parser.add_argument("--" + flag, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, default=30900)
    parser.add_argument("--base-port", type=int, default=31000)
    parser.add_argument("--legacy-withdrawal-receipts", action="store_true")
    parser.add_argument("--checkpoint-compact-transactions", action="store_true")
    parser.add_argument("--checkpoint-live-cell-replay", action="store_true")
    args = parser.parse_args()
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "3s", 1, 1
    args.checkpoint_compact_transactions |= args.checkpoint_live_cell_replay
    if args.checkpoint_compact_transactions:
        args.checkpoint_interval, args.checkpoint_cell_proofs = 4, True
    args.output.mkdir(mode=0o700)
    fixture = Fixtures(args)
    l1 = L1(args)
    try:
        scenario(args, l1, fixture)
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

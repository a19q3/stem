#!/usr/bin/env python3
"""Use real CKB Stake Cells to change the Jolteon committee and restart a lagging observer."""

import argparse
import json
from pathlib import Path
import struct
import subprocess
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import CKB, FEE, Fixtures, L1, binary, ckb_hash, hex_bytes, le, point, script_hash, write_key
from jolteon_bridge_localnet import Client, fee_payment
from jolteon_checkpoint_localnet import Relay, exercise_publisher
from checkpoint_fee_fund_localnet import funded_handoff
from jolteon_localnet import FINALITY, HANDOFF, HEADER, STATUS, SUBMIT, rpc, write_configuration
from jolteon_slash_localnet import evidence as slash_evidence
from localnet import run, write_json
from native_bridge_localnet import bridge_configuration, deploy, deposit_and_mint, redeem_withdrawal, start, wait_for
from stake_localnet import CHAIN, DELAY, EMPTY, Scenario, mk_record, proof, signable


def public(key):
    return key.public_key().public_bytes_raw()


def retirement_marker(client, validator, effective):
    response = client.query("/stem/stake-retirement", data=hex_bytes(validator), prove=True)
    marker = json.loads(bytes.fromhex(response["value"]))
    assert marker["effective_height"] == effective
    return marker, response["height"]


def registry_application(client, registry_id, sequence):
    data = hex_bytes(registry_id + le(sequence, 8))
    response = client.query("/stem/stake-registry-application", data=data, prove=True)
    return json.loads(bytes.fromhex(response["value"])), response["height"]


def publish_handoff(args, scenario, relay, epoch):
    path = args.output / f"handoff-{epoch}.json"
    write_json(path, rpc(relay.client.port, HANDOFF, le(epoch, 8))["proof"])
    return relay.advance((epoch + 1) * 10, scenario.secp, proof_file=path, terminal=True)


def confirm_positions(args, scenario, relay, validator, genesis, next_registry):
    publish_handoff(args, scenario, relay, 0)
    first, _ = registry_application(relay.client, scenario.registry_id, genesis["sequence"])
    following, height = registry_application(relay.client, scenario.registry_id, next_registry["sequence"])
    assert first["effective_height"] == 1 and following["effective_height"] == 11
    checkpoint = relay.advance(height, scenario.secp)
    wrong = scenario.delegation("claim-preexisting-activation", 16, validator, label="top-up",
        application=first, checkpoint=checkpoint, funded_by_owner=False, expected="error code 23")
    applications = [("holder", first), ("top-up", following), ("second", following)]
    if args.reward_accounting:
        applications.append(("genesis-fragment", first))
    confirmed = [scenario.delegation(f"activate-{label}", 16, validator, label=label, application=application,
        checkpoint=checkpoint, funded_by_owner=False)
        for label, application in applications]
    return {"first": first, "next": following, "confirmations": confirmed, "wrong_interval_rejected": wrong}


def reward_epoch(client, verify, epoch):
    response = client.query("/stem/stake-reward-epoch", data=hex_bytes(le(epoch, 8)), prove=True)
    height = response["height"]
    finality = client.finality(height)
    checked = verify(finality["proof"], f"reward-epoch-{epoch}-finality",
                     range(finality["epoch"]), terminal=finality["handoff"])
    assert checked == client.header(height) and checked["height"] == height
    return json.loads(bytes.fromhex(response["value"])), checked


def validate_checkpoint_policy(args, l1, fixture, scenario, spec, vault):
    alternate = deploy(args, l1, fixture, spec["network_id"], vault["change_point"],
        checkpoint_anchor=scenario.foreign_checkpoint, prefix="substitute-")
    assert alternate["anchor"]["id"] != vault["anchor"]["id"]

    def rejected(name, draft, reason):
        path = args.output / f"reject-{name}-genesis.json"
        write_json(path, draft)
        failure = subprocess.run([args.binary, "genesis-hash", "--genesis", path], text=True, capture_output=True)
        assert failure.returncode and reason in failure.stderr, failure
        return failure.stderr

    lane = {"vault_type": alternate["type"], "vault": point(alternate["vault"], 0)}
    forked = dict(spec, bridge=dict(spec["bridge"], lanes=[lane]))
    wrong_vault = rejected("different-vault-checkpoint", forked, "same CKB checkpoint")
    trust = json.loads((args.output / "snapshot-trust.json").read_text())
    data = binary(trust["registry_type"]["args"])
    registry_type = dict(trust["registry_type"], args=hex_bytes(data[:152] + alternate["anchor"]["id"] + data[184:]))
    write_json(args.output / "substitute-registry-trust.json", dict(trust, registry_type=registry_type))
    wrong_registry = rejected("substituted-registry", dict(spec, stake=dict(spec["stake"], trust="substitute-registry-trust.json")),
        "genesis observation identifies a different network or epoch")
    wrong_chain = rejected("different-chain-id", dict(spec, chain_id="wrong-chain"), "Stake Registry policies differ")
    return {"registry_checkpoint": hex_bytes(vault["anchor"]["id"]), "substitute_checkpoint": hex_bytes(alternate["anchor"]["id"]),
            "substitute_vault": alternate["vault"], "substitute_vault_inclusion": l1.prove_committed(alternate["vault"]),
            "vault_rejected": wrong_vault, "registry_substitution_rejected": wrong_registry, "chain_id_rejected": wrong_chain}


def reward_quotes(args, scenario, client, verify, genesis, committee, key, metadata, delegations):
    epoch, checked = reward_epoch(client, verify, 0)
    assert int(epoch["budget"], 16) == 520 * CKB, epoch
    header = args.output / "reward-authenticated-header.json"
    write_json(header, hex_bytes(bytes.fromhex(checked["header"])))
    roster = (le(0, 8) + le(1, 8) + b"".join(binary(member["ed25519"]) + le(member["power"], 8) for member in committee))
    common = {"epoch_record": epoch["record"], "proof": epoch["proof"],
              "registry": genesis["registry_data"], "committee": hex_bytes(roster)}

    def quote(name, details):
        request = args.output / f"reward-{name}.json"
        write_json(request, dict(common, subject=details))
        command = [args.binary, "stake", "reward-quote", "--network", hex_bytes(scenario.network),
                   "--header", header, "--request", request]
        return json.loads(run(*command))

    validator = {"kind": "validator", "validator": hex_bytes(public(key)), "metadata": hex_bytes(metadata)}
    subjects = [{"kind": "delegation", "identity": entry["identity"], "principal": entry["proof"]["principal"],
                 "proof": entry["proof"]["proof"]} for entry in delegations]
    operator = quote("operator", validator)
    delegates = [quote(f"delegate-{index}", subject) for index, subject in enumerate(subjects)]
    assert operator["shannons"] == 38_999_999_999 and operator["commission_shannons"] == 1_857_142_857
    assert [delegate["shannons"] for delegate in delegates] == [5_200_000_000, 7_800_000_000]
    assert operator["shannons"] + sum(delegate["shannons"] for delegate in delegates) == 520 * CKB - 1
    assert operator["recipient_lock_hash"] == hex_bytes(script_hash(scenario.fixture.standard))
    assert all(delegate["recipient_lock_hash"] == hex_bytes(script_hash(scenario.fixture.friend_lock))
               for delegate in delegates)
    forged = args.output / "reward-forged-position.json"
    subject = subjects[0]
    write_json(forged, dict(common, subject=dict(subject, principal=subject["principal"] - 1)))
    bad = subprocess.run([args.binary, "stake", "reward-quote", "--network", hex_bytes(scenario.network),
                          "--header", header, "--request", forged], text=True, capture_output=True)
    assert bad.returncode != 0 and "Sequence" in bad.stderr, bad.stderr
    return {"height": checked["height"], "epoch": epoch, "operator": operator, "delegates": delegates,
            "authenticated_header": checked,
            "inflated_or_altered_principal_rejected": bad.stderr}


def reward_payouts(args, l1, fixture, vault, spec, spec_path, lane, client, relay, verify, recover, rewards):
    quotes = rewards["quotes"]
    mint = rewards["mint"]

    def proved(path, data="0x"):
        reply = client.query(path, data=data, prove=True)
        return json.loads(bytes.fromhex(reply["value"]))

    def request(recipient, positions, payout=None):
        if payout is None:
            payout = sum(quote["shannons"] for _, quote in positions)
        entries = [json.loads((args.output / f"reward-{name}.json").read_text()) for name, _ in positions]
        epochs = [{"epoch": 0, "registry": entries[0]["registry"], "committee": entries[0]["committee"],
                   "subjects": [entry["subject"] for entry in entries]}] if entries else []
        return {"payout": payout, "recipient": recipient, "epochs": epochs}

    def build_claim(name, claims):
        draft = args.output / f"reward-{name}-claim.json"
        write_json(draft, claims)
        return json.loads(run(args.binary, "stake", "claim-rewards", "--request", draft))

    def rejected(tx, message):
        reply = client.call("broadcast_tx_sync", tx=tx)
        assert reply["code"] == 1 and message in reply["log"], reply
        return reply["log"]

    def accepted(name, draft):
        tx = build_claim(name, draft)
        sent = client.call("broadcast_tx_commit", tx=tx)
        assert sent["check_tx"]["code"] == 0, sent
        return sent, len(binary(tx))

    def authenticate_balance(label, recipient):
        owner = hex_bytes(script_hash(recipient))
        response = client.query("/stem/stake-reward-balance", data=owner, prove=True)
        checked_height = response["height"]
        finality = client.finality(checked_height)
        header = verify(finality["proof"], f"reward-{label}-balance-finality",
                        range(finality["epoch"]), terminal=finality["handoff"])
        assert header == client.header(checked_height)
        path = args.output / f"reward-{label}-balance-header.json"
        write_json(path, "0x" + header["header"])
        record = json.loads(bytes.fromhex(response["value"]))
        proof = args.output / f"reward-{label}-balance-proof.json"
        write_json(proof, record)
        command = [args.binary, "stake", "verify-reward-balance", "--network", spec["network_id"],
                   "--owner", owner, "--header", path]
        checked = json.loads(run(*command, "--proof", proof))
        assert checked["balance_shannons"] == int(record["balance_shannons"], 16)
        return record, command

    validator = [("operator", quotes["operator"])]
    delegation = [(f"delegate-{index}", quote) for index, quote in enumerate(quotes["delegates"])]
    operator_claim = request(fixture.standard, validator)
    delegation_claim = request(fixture.friend_lock, delegation)
    permissions = {
        "aggregate": rejected(json.loads(run(args.binary, "bridge", "claim-fees", "--fund", "0",
            "--amount", str(520 * CKB), "--claimed-before", "0")), "requires individual reward proofs"),
        "wrong_recipient": rejected(build_claim("wrong-recipient", dict(delegation_claim, recipient=fixture.standard)),
                                    "reward recipient does not own the position"),
        "wrong_amount": rejected(build_claim("wrong-amount", dict(delegation_claim, payout=delegation_claim["payout"] + 1)),
                                 "differs from the authenticated balance"),
        "dust": rejected(build_claim("dust", request(fixture.friend_lock, delegation[:1])),
                         "below L1 occupied Capacity"),
    }
    forged = json.loads((args.output / "reward-forged-position.json").read_text())["subject"]
    groups = delegation_claim["epochs"]
    permissions["forged_principal"] = rejected(
        build_claim("altered-principal", dict(delegation_claim, epochs=[
            dict(groups[0], subjects=[forged, groups[0]["subjects"][1]])
        ])), "Sequence")
    credits = []
    accrued = 0
    for index, position in enumerate(delegation):
        credit = request(fixture.friend_lock, [position], payout=0)
        result, size = accepted(f"accrue-{index}", credit)
        accrued += position[1]["shannons"]
        receipt, verify_balance = authenticate_balance(f"accrued-{index}", fixture.friend_lock)
        backing = proved("/stem/bridge-backing")
        funds = proved("/stem/fee-funds")["accounts"][0]
        assert int(receipt["balance_shannons"], 16) == accrued
        assert int(backing["accrued_stake_rewards_shannons"], 16) == accrued
        assert int(backing["unallocated_shannons"], 16) == 800 * CKB - accrued
        assert int(backing["liquid_shannons"], 16) == mint["capacity"]
        assert int(backing["pending_shannons"], 16) == 0
        assert int(funds["accrued_shannons"], 16) == accrued
        assert int(funds["queued_shannons"], 16) == 0
        pending = proved("/stem/bridge-withdrawal", hex_bytes(binary(lane) + le(0, 8)))
        assert pending["record"] is None
        credits.append({"transaction": result, "balance": receipt, "backing": backing, "fund": funds,
                        "position": position[1], "transaction_bytes": size})
        permissions[f"accrual_{index}_replay"] = rejected(
            build_claim(f"accrue-{index}-replay", dict(credit, payout=accrued)), "already claimed")
        if index == 0:
            invalid = args.output / "reward-tampered-balance-proof.json"
            write_json(invalid, dict(receipt, balance_shannons=hex(accrued + 1)))
            tampered = subprocess.run([*verify_balance, "--proof", invalid], capture_output=True, text=True)
            assert tampered.returncode and "not authenticated" in tampered.stderr, tampered
            permissions["altered_balance_proof"] = tampered.stderr
            permissions["premature_payout"] = rejected(
                build_claim("premature-payout", request(fixture.friend_lock, [], payout=accrued)),
                "below L1 occupied Capacity")
    recovery = recover()
    restarted_balance, _ = authenticate_balance("after-restart", fixture.friend_lock)
    assert int(restarted_balance["balance_shannons"], 16) == accrued
    assert int(proved("/stem/bridge-backing")["accrued_stake_rewards_shannons"], 16) == accrued
    permissions["stale_balance"] = rejected(
        build_claim("stale-balance", request(fixture.friend_lock, [], payout=delegation[0][1]["shannons"])),
        "differs from the authenticated balance")
    delegation_claim = request(fixture.friend_lock, [], payout=accrued)
    payouts = []
    input_vault, fee_input = point(mint["l1_consolidation"], 0), point(mint["l1_consolidation"], 1)
    previous = mint
    for nonce, (label, claim, positions) in enumerate((("validator", operator_claim, validator),
                                                       ("delegator", delegation_claim, delegation))):
        sent, size = accepted(label, claim)
        alternate = build_claim(f"{label}-replay", dict(claim, payout=claim["payout"] + 1))
        expected = "already claimed" if label == "validator" else "differs from the authenticated balance"
        permissions[f"{label}_replay"] = rejected(alternate, expected)
        # The permanent claim marker survives the L1 settlement.
        payout = redeem_withdrawal(args, l1, fixture, vault, spec, spec_path, lane, client, relay,
            name=f"reward-{label}", burned=sent, recipient=claim["recipient"], capacity=claim["payout"],
            units=b"", nonce=nonce, input_vault=input_vault, fee_input=fee_input, previous_anchor=previous)
        output = l1.call("get_transaction", payout["l1_payout"])["transaction"]["outputs"][0]
        assert output["lock"] == claim["recipient"] and int(output["capacity"], 16) == claim["payout"]
        leftover, _ = authenticate_balance(f"after-{label}", fixture.friend_lock)
        assert int(leftover["balance_shannons"], 16) == (accrued if label == "validator" else 0)
        marks = []
        for index, (_, quote) in enumerate(positions):
            key = hex_bytes(le(quote["epoch"], 8) + bytes([quote["kind"]]) + binary(quote["position_id"]))
            mark = proved("/stem/stake-reward-claim", key)
            source = sent if label == "validator" else credits[index]["transaction"]
            assert mark["record"] == hex_bytes(binary(source["hash"]) + le(quote["shannons"], 8))
            marks.append(mark)
        payouts.append({"payout": payout, "positions": marks, "transaction_bytes": size, "delegator_balance": leftover})
        input_vault, fee_input = point(payout["l1_payout"], 1), point(payout["l1_payout"], 2)
        previous = payout
    permissions["rebundled_after_l1_payment"] = rejected(
        build_claim("rebundled-replay", request(fixture.friend_lock, delegation[1:])), "already claimed")
    funds, backing = proved("/stem/fee-funds"), proved("/stem/bridge-backing")
    allocated = 520 * CKB - 1
    validator_account = funds["accounts"][0]
    assert int(validator_account["queued_shannons"], 16) == int(validator_account["paid_shannons"], 16) == allocated
    assert int(backing["liquid_shannons"], 16) == mint["capacity"] - allocated
    assert int(backing["circulating_shannons"], 16) == mint["capacity"] - 800 * CKB
    assert int(backing["pending_shannons"], 16) == 0
    assert int(backing["accrued_stake_rewards_shannons"], 16) == 0
    assert int(backing["unallocated_shannons"], 16) == 280 * CKB + 1
    return {"credits": credits, "recovery": recovery, "balance_after_restart": restarted_balance,
            "payouts": payouts, "rejections": permissions, "funds": funds, "backing": backing,
            "bridge_checkpoints": relay.report()}


def exercise_reductions(args, l1, scenario, relay, key, incoming, registry, requested, joined):
    validator = public(key)
    sequence = registry["sequence"]
    application, height = registry_application(relay.client, scenario.registry_id, sequence)
    assert application["effective_height"] == 21 and application["l1_hash"] == registry["block_hash"]
    assert application["previous_sequence"] < requested["sequence"] <= sequence
    checkpoints = {}
    replacement = None
    if args.reduction_reorg:
        anchor_height = application["l1_height"]
        l1.call("truncate", l1.call("get_block_hash", hex(anchor_height - 1)))
        l1.mine(3)
        replacement = l1.call("get_block_hash", hex(anchor_height))
        assert replacement != application["l1_hash"], application
        for _, _, point, _ in scenario.delegations.values():
            assert l1.call("get_live_cell", point, True)["status"] == "live", point
    early = publish_handoff(args, scenario, relay, 1)
    assert relay.header(20)[112:144] == relay.header(height)[112:144]
    if replacement is None:
        checkpoints["not_effective"] = scenario.delegation("confirm-before-effective", 13, validator,
            application=application, checkpoint=early, funded_by_owner=False, expected="error code 23")
    checkpoint = relay.advance(height, scenario.secp)
    if replacement is not None:
        orphaned = scenario.delegation("confirm-orphaned-l1", 13, validator,
            application=application, checkpoint=checkpoint, funded_by_owner=False, expected="InvalidHeader")
        wrong = scenario.delegation("confirm-replacement-l1", 13, validator,
            application=dict(application, l1_hash=replacement), checkpoint=checkpoint,
            funded_by_owner=False, expected="error code 23")
        return {"joined": joined, "application": application, "requested": requested,
                "proof_height": height, "replacement_l1_hash": replacement,
                "orphaned_anchor_rejected": orphaned, "substituted_anchor_rejected": wrong,
                "checkpoint": relay.report()}
    checkpoints["wrong_anchor"] = scenario.delegation("confirm-missing-l1-anchor", 13, validator,
        application=application, checkpoint=checkpoint, omit_l1_anchor=True, funded_by_owner=False,
        expected="error code 23")
    checkpoints["tampered_cutoff"] = scenario.delegation("confirm-invalid-height", 13, validator,
        application=dict(application, effective_height=20), checkpoint=checkpoint, funded_by_owner=False,
        expected="error code 23")
    confirmations = [scenario.delegation(f"confirm-{label}", 13, validator, label=label,
        application=application, checkpoint=checkpoint, funded_by_owner=False)
        for label in ("holder", "second")]
    checkpoints["repeated_confirmation"] = scenario.delegation("confirm-twice", 13, validator,
        application=application, checkpoint=checkpoint, funded_by_owner=False, expected="error code 23")
    checkpoints["immature"] = scenario.delegation("withdraw-before-delay", 14, validator,
        funded_by_owner=False, expected="Immature")
    l1.mine(DELAY)
    checkpoints["recipient_redirect"] = scenario.delegation("withdraw-wrong-lock", 14, validator,
        funded_by_owner=False, wrong_recipient=True, expected="error code 23")
    settlement = []
    historical = None
    if args.reduction_slash:
        wait_for(lambda: relay.client.status()["height"] >= 24, "post-cutoff double-vote fixture")
        target = relay.client.status()["height"]
        checkpoint = relay.advance(target, scenario.secp)
        late, _, _, _ = slash_evidence(args, relay, key, 2, 22, target)
        write_json(args.output / "reduction-late-evidence.json", hex_bytes(late))
        checkpoints["post_cutoff_vote"] = scenario.delegation("pending-slash-after-cutoff", 15, validator,
            evidence=late, checkpoint=checkpoint, funded_by_owner=False, expected="error code 23")
        old, forged, _, penalty = slash_evidence(args, relay, key, 0, 3, target, alternative_height=4)
        historical = hex_bytes(old)
        checkpoints["before_new_position"] = scenario.delegation("pending-slash-before-activation", 15,
            validator, label="second", evidence=old, checkpoint=checkpoint, funded_by_owner=False, expected="error code 23")
        checkpoints["invalid_historical_signature"] = scenario.delegation("pending-slash-forged", 15,
            validator, evidence=forged, checkpoint=checkpoint, funded_by_owner=False, expected="error code 25")
        stake = scenario.records[validator][1]
        frozen = mk_record(validator, int.from_bytes(stake[:8], "little"), 0, None,
            stake[24:72] + penalty, frozen=True, fault_height=4, fault_start_height=3)
        invalid = mk_record(validator, int.from_bytes(stake[:8], "little"), 0, None,
            stake[24:72] + penalty, frozen=True, fault_height=4)
        checkpoints["freeze_omits_earlier_vote_height"] = scenario.change("freeze-wrong-first-height",
            6, validator, invalid, lambda *_: old, dependencies=[checkpoint], expected="error code 23")
        tx = scenario.change("freeze-older-than-second-position", 6, validator, frozen,
            lambda *_: old, dependencies=[checkpoint], measure=True)
        checkpoints["withdraw_liable_position"] = scenario.delegation("withdraw-liable-old-position", 14,
            validator, funded_by_owner=False, expected="error code 23")
        settlement.append(scenario.delegation("slash-reduced-holder", 15, validator, evidence=old,
            checkpoint=checkpoint, funded_by_owner=False))
        late_freeze = {"transaction": tx, "proof": l1.prove_committed(tx)}
    else:
        late_freeze = None
        settlement.append(scenario.delegation("withdraw-reduced-holder", 14, validator, funded_by_owner=False))
    settlement.append(scenario.delegation("withdraw-full-second", 14, validator, label="second", funded_by_owner=False))
    assert scenario.delegations["holder"][1] == (35 * CKB, 0, 0, 0, 0, 1)
    scenario.delegate_fee_point = settlement[-1]["recipient_output"]
    scenario.delegate_fee_capacity = settlement[-1]["recipient_paid"]
    switched = scenario.delegation("redelegate-withdrawn-capital", 9, public(incoming), 8 * CKB,
        label="redelegated", separate_owner=True)
    active = relay.client.status()
    assert active["epoch"] == 2 and joined[0]["voting_power"] == 360
    return {"joined": joined, "application": application, "initial_request": requested,
            "proof_height": height, "confirmations": confirmations, "settlement": settlement,
            "rejections": checkpoints, "historical_evidence": historical, "checkpoint": relay.report(),
            "global_freeze": late_freeze, "redelegated": switched, "active_status_after_settlement": active,
            "measurements": scenario.measurements}


def freeze_fixture(args, scenario, relay, signer, epoch, sample_height, checkpoint_height, old_meta, low,
                   exit_epoch, dependency):
    validator = public(signer)
    raw, forged, late, penalty = slash_evidence(args, relay, signer, epoch, sample_height, checkpoint_height)
    frozen_meta = old_meta[:48] + penalty
    frozen_record = mk_record(validator, low, 0, exit_epoch, frozen_meta, frozen=True, fault_height=sample_height)
    failures = {
        "forged_vote": scenario.change("freeze-forged-vote", 6, validator, frozen_record,
            lambda *_: forged, dependencies=[dependency], expected="error code 25"),
        "outside_committee_interval": scenario.change("freeze-late-vote", 6, validator, frozen_record,
            lambda *_: late, dependencies=[dependency], expected="error code 23"),
        "freeze_missing_checkpoint": scenario.change("freeze-no-checkpoint", 6, validator, frozen_record,
            lambda *_: raw, expected="error code 23"),
    }
    tx = scenario.change("freeze-double-vote", 6, validator, frozen_record,
        lambda *_: raw, dependencies=[dependency], measure=True, relayer=True)
    failures["replayed_double_vote"] = scenario.change("freeze-replay", 6, validator, frozen_record,
        lambda *_: raw, dependencies=[dependency], expected="error code 23")
    failures["slash_before_retirement"] = scenario.change("slash-before-retire", 8, validator,
        mk_record(validator, 0, 0, exit_epoch, frozen_meta, frozen=True), fee=low + FEE, expected="error code 23")
    return tx, frozen_meta, failures


def publish_checkpoints(args, l1, scenario, relay, spec, client, target_height, exit_case, active_freeze):
    frozen, slashed = None, None
    retired = None
    failures = {}
    since = (1 << 63) | DELAY
    if exit_case:
        signer, old_meta, low, effective, early_marker = exit_case
        validator = public(signer)
        epoch = None if active_freeze else (effective - 1) // 10
        if active_freeze:
            frozen, old_meta, failures = active_freeze
        old_record = mk_record(validator, 0, 0, epoch, old_meta, True, bool(frozen))
        retired_meta = old_meta[:40] + le(effective, 8) + old_meta[48:]
        retired_record = mk_record(validator, low, 0, epoch, retired_meta, frozen=bool(frozen))
        final_record = mk_record(validator, 0, 0, epoch, retired_meta, True, bool(frozen))

    for height, label in ((10, "handoff-0"), (20, "handoff-1"), (target_height, "post-handoff")):
        if relay.publications and height <= relay.publications[-1]["height"]:
            continue
        dependency = relay.advance(height, scenario.secp, proof_file=args.output / f"{label}.json",
                                   terminal=height != target_height, probe=height in (10, target_height))
        if exit_case and height == effective - 1:
            failures["not_yet_inactive"] = scenario.change("retire-early", 4, validator, retired_record,
                lambda *_: binary(early_marker["proof"]), dependencies=[dependency], expected="error code 23")
            l1.mine(DELAY)
            failures["withdraw_without_retirement"] = scenario.change("unretired-withdrawal", 5, validator, old_record,
                since=since, expected="error code 23")
    if exit_case:
        if args.slash and not frozen:
            sample_epoch = 0 if args.replace else 1
            frozen, frozen_meta, rejected = freeze_fixture(args, scenario, relay, signer, sample_epoch,
                3 if args.replace else 12, target_height, old_meta, low, epoch, dependency)
            failures.update(rejected)
            retired_meta = retired_meta[:48] + frozen_meta[48:]
            retired_record = mk_record(validator, low, 0, epoch, retired_meta, frozen=True)
            final_record = mk_record(validator, 0, 0, epoch, retired_meta, True, True)
        marker, captured = retirement_marker(client, validator, effective)
        # Empty execution after the last announcement preserves the SystemRoot.
        assert relay.header(captured)[112:144] == relay.header(target_height)[112:144]
        marker_file = args.output / "effective-retirement-proof.json"
        header_file = args.output / "effective-retirement-header.json"
        write_json(marker_file, marker)
        write_json(header_file, hex_bytes(relay.header(target_height)))
        witness = binary(json.loads(run(args.binary, "stake", "retirement-witness", "--genesis", spec,
            "--validator", hex_bytes(validator), "--marker", marker_file, "--header", header_file)))
        failures["missing_checkpoint"] = scenario.change("retire-no-checkpoint", 4, validator, retired_record,
            lambda *_: witness, expected="error code 23")
        failures["modified_path"] = scenario.change("retire-bad-path", 4, validator, retired_record,
            lambda *_: witness[:-1] + bytes([witness[-1] ^ 1]), dependencies=[dependency], expected="error code 23")
        retirement = scenario.change("retire-excluded", 4, validator, retired_record, lambda *_: witness,
            dependencies=[dependency], relayer=True, measure=True)
        failures["retirement_replayed"] = scenario.change("retirement-replay", 4, validator, retired_record,
            lambda *_: witness, dependencies=[dependency], expected="error code 23")
        if frozen:
            l1.mine(DELAY)
            failures["withdraw_before_slash"] = scenario.change("unbond-before-slash", 5, validator, final_record,
                since=since, expected="error code 23")
            slash_record = mk_record(validator, 0, 0, epoch, retired_meta, frozen=True)
            failures["unfunded_slash"] = scenario.change("slash-unfunded", 8, validator, slash_record,
                expected="error code 23")
            slashed = scenario.change("finalize-slash", 8, validator, slash_record, fee=low + FEE, measure=True)
        delegated = []
        if args.delegation:
            if not frozen:
                failures["delegation_immature"] = scenario.delegation("delegation-immature", 10, validator,
                    expected="Immature", funded_by_owner=False)
        else:
            failures["immature"] = scenario.change("early-unbonding", 5, validator, final_record,
                since=since, expected="Immature")
        l1.mine(DELAY)
        if args.delegation:
            failures["registry_release_leaves_delegations"] = scenario.change("withdraw-with-delegations", 5,
                validator, final_record, since=since, expected="error code 23")
            action = 11 if frozen else 10
            failures["wrong_delegation_recipient"] = scenario.delegation("redirect-delegation-release", action,
                validator, expected="error code 23", wrong_recipient=True, funded_by_owner=False)
            if not frozen:
                failures["invalid_delegation_clock"] = scenario.delegation("delegation-clock", 10, validator,
                    since=0, expected="error code 23", funded_by_owner=False)
            else:
                failures["withdraw_frozen_delegation"] = scenario.delegation("withdraw-frozen-delegation", 10,
                    validator, expected="error code 23", funded_by_owner=False)
            for label in list(scenario.delegations):
                delegated.append(scenario.delegation(f"settle-{label}", action, validator,
                    label=label, funded_by_owner=False))
        failures["owner_required"] = scenario.change("unbond-by-relayer", 5, validator, final_record,
            since=since, expected="error code 23", relayer=True)
        unbonded = scenario.change("unbond-retired", 5, validator, final_record, since=since, measure=True)
        assert validator not in scenario.records and validator in scenario.retired
        retired = {"effective_height": effective, "retirement": retirement, "unbonding": unbonded,
                   "bond_shannons": low, "retirement_l1_proof": l1.prove_committed(retirement),
                   "unbonding_l1_proof": l1.prove_committed(unbonded), "rejections": failures,
                   "measurements": scenario.measurements, "freeze": frozen, "slash": slashed,
                   "delegations": delegated,
                   "released_bond": 0 if slashed else low, "slashed_bond": low if slashed else 0}
        if slashed:
            retired["frozen_before_snapshot"] = bool(active_freeze)
            retired["slash_l1_proof"] = l1.prove_committed(slashed)
            retired["freeze_l1_proof"] = l1.prove_committed(frozen)
    return relay.report(), retired


def run_epochs(args, l1, scenario, fixture):
    old, incoming, observer = [Ed25519PrivateKey.from_private_bytes(bytes([seed]) * 32)
                               for seed in (31, 32, 33)]
    low, high = 250 * CKB, 400 * CKB
    owner = script_hash(fixture.standard)

    def activate(key, amount, epoch):
        pub = public(key)

        def message(domain, previous, next_):
            return signable(domain, scenario.network, scenario.genesis, scenario.registry_id, previous, next_)

        registered = scenario.change(f"register-{epoch}", 0, pub,
            mk_record(pub, amount, epoch, None, owner + bytes(48), commission=1250 if args.delegation else 0),
            lambda before, after: key.sign(message(b"Stem CKB Stake registration\0", before, after)))
        meta = owner + le(1 + epoch * 10, 8) + le(0, 8) + EMPTY
        activated = scenario.change(f"activate-{epoch}", 3, pub, mk_record(pub, amount, epoch, None, meta),
            lambda before, after: proof(scenario.authority, message(b"Stem CKB Stake activation\0", before, after)))
        return registered, activated, meta

    registered_old, activated_old, old_meta = activate(old, low, 0)
    delegation = None
    genesis_positions = []
    if args.delegation:
        key = public(old)
        wrong_owner = scenario.delegation("unauthorized-delegation", 9, key, 100 * CKB,
            expected="error code 23", funded_by_owner=False)
        fake_weight = scenario.delegation("inflated-delegation-weight", 9, key, 100 * CKB,
            expected="error code 23", registry_delta=120 * CKB)
        wrong_identity = scenario.delegation("substituted-delegation-id", 9, key, 100 * CKB,
            expected="error code 23", wrong_identity=True)
        wrong_proof = scenario.delegation("invalid-delegation-principal-proof", 9, key, 100 * CKB,
            expected="error code 23", wrong_principal_proof=True)
        start_bond = scenario.delegation("first-delegation", 9, key, (40 if args.reward_accounting else 100) * CKB)
        delegation = {"bond": start_bond, "rejections": {"unauthorized": wrong_owner, "inflated": fake_weight,
                      "identity": wrong_identity, "principal_proof": wrong_proof}}
        if args.reward_accounting:
            delegation["genesis_fragment"] = scenario.delegation("second-genesis-delegation", 9, key,
                60 * CKB, label="genesis-fragment")
            for label in ("holder", "genesis-fragment"):
                identity = scenario.delegations[label][0]["identity"]
                genesis_positions.append({"identity": identity,
                    "proof": scenario.delegation_proof(f"epoch-zero-{label}", identity)})
    genesis = scenario.observe("initial", 0, count=3)
    verified, _ = scenario.verify_genesis_snapshot(genesis)
    genesis_committee = verified["validators"]
    assert genesis_committee == [{"ed25519": hex_bytes(public(old)), "power": 350 if delegation else 250}]
    registered_new, activated_new, new_meta = activate(incoming, high, 1)
    if delegation:
        delegation["top_up"] = scenario.delegation("top-up-delegation", 9, public(old), 75 * CKB, label="top-up")
        delegation["separate_position"] = scenario.delegation("second-delegation", 9, public(old), 70 * CKB,
            label="second", separate_owner=True)
        delegation["rejections"]["before_retirement"] = scenario.delegation("active-delegation-withdraw", 10,
            public(old), since=0, expected="error code 23", funded_by_owner=False)

    spec = args.output / "stem-genesis.json"
    network_spec = {
        "chain_id": CHAIN.decode(), "network_id": hex_bytes(scenario.network), "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024, "transaction_cycles": 20_000_000,
                   "block_cycles": 400_000_000, "cell_reads": 1024, "cell_read_bytes": 64 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "stake": {"trust": "snapshot-trust.json", "observations": "genesis-observations.json", "signing_window": 5,
                  "limits": {"operations": 1, "bytes": 256 * 1024, "signatures": 8}},
    }
    vault = lane = checkpoint_policy = None
    if args.reward_accounting:
        args.xudt = None
        funding = dict(scenario.code["out_point"], index="0x2")
        vault = deploy(args, l1, fixture, hex_bytes(scenario.network), funding, checkpoint_anchor=scenario.checkpoint)
        bridge, lane = bridge_configuration(args, l1, fixture, vault, None, hex_bytes(scenario.network))
        bridge["fee_funds"] = [{"basis_points": share, "recipient": dict(
            fixture.standard, args=hex_bytes(ckb_hash(f"reward-fixture-{index}".encode())[:20]))}
            for index, share in enumerate((6500, 1500, 1000, 1000))]
        network_spec["bridge"] = bridge
        checkpoint_policy = validate_checkpoint_policy(args, l1, fixture, scenario, network_spec, vault)
    write_json(spec, network_spec)
    write_json(args.output / "genesis-committee.json", genesis_committee)
    write_json(args.output / "public-genesis-header.json",
               json.loads(run(args.binary, "genesis-hash", "--genesis", spec, "--header")))
    keys = sorted([old, incoming, observer], key=public)
    ports = {public(key): args.port + i for i, key in enumerate(keys)}
    cadence, timeout = (2400, 5200) if args.reward_accounting else (1700, 4000) if args.delegation else (1400, 3600) if args.active_slash else (750, 2800)
    configuration = write_configuration(args.output, [public(key) for key in keys], ports.values(), cadence, timeout)
    key_files = {public(key): write_key(args.output, f"jolteon-{i}", key.private_bytes_raw())
                 for i, key in enumerate(keys)}
    processes = {}

    def launch(key, initialize, home=None):
        pub = public(key)
        data_dir = home or args.output / f"node-{ports[pub]}"
        command = [args.binary, "run", "--home", data_dir, "--genesis", spec,
                   "--configuration", configuration, "--key-file", key_files[pub]]
        if pub == public(incoming):
            command.append("--archive")
        else:
            command += ["--history-window", "64"]
        phase = "initial" if initialize else "snapshot-restart" if home else "restart"
        if initialize:
            command.append("--initialize")
        processes[pub] = start(args, command, f"stem-{ports[pub]}-{phase}.log")

    def request(key, kind, body=b""):
        pub = public(key)
        code = processes[pub].poll()
        if code is not None:
            raise AssertionError(f"node {ports[pub]} exited with code {code}")
        return rpc(ports[pub], kind, body)

    def progress(key, height, epoch, seconds=65):
        def reached():
            reply = request(key, STATUS)
            return reply if reply["height"] >= height and reply["epoch"] == epoch else None

        return wait_for(reached, f"node {ports[public(key)]} epoch {epoch} height {height}", seconds=seconds)

    def header(key, height):
        return request(key, HEADER, struct.pack("<Q", height))

    def snapshot(epoch, submitter, parent):
        signing_height = 10 * epoch - 5
        progress(submitter, signing_height, epoch - 1)
        anchor = hex_bytes(ckb_hash(b"Stem Header\0" + scenario.network + bytes.fromhex(header(submitter, signing_height)["header"])))
        votes = scenario.observe(f"epoch-{epoch}", epoch, parent, count=3, signing_anchor=anchor)
        attestations = args.output / f"epoch-{epoch}-votes.json"
        write_json(attestations, votes)
        tx = binary(json.loads(run(args.binary, "stake", "snapshot", "--observations", attestations)))
        reply = request(submitter, SUBMIT, tx)
        assert "admitted" in reply, reply
        write_json(args.output / f"epoch-{epoch}-transaction.json", hex_bytes(tx))
        return votes[0]["registry"]

    def verify(raw, label, prior=(), terminal=False):
        path = args.output / f"{label}.json"
        write_json(path, raw)
        command = [args.binary, "jolteon-proof", "--genesis", spec, "--proof", path]
        for epoch in prior:
            command += ["--handoff", args.output / f"handoff-{epoch}.json"]
        if terminal:
            command.append("--terminal")
        checked = json.loads(run(*command))
        return checked

    try:
        for key in keys:
            launch(key, True)
        initial = [progress(key, 3, 0) for key in (old, incoming, observer)]
        assert initial[0]["member"] == 0 and all(status["member"] is None for status in initial[1:])
        assert [status["voting_power"] for status in initial] == [350 if delegation else 250, None, None]
        rewards = None
        if vault:
            client = Client(args, ports[public(old)], hex_bytes(scenario.network))
            mint = deposit_and_mint(args, l1, fixture, vault, network_spec, spec, lane, client)
            payment = fee_payment(args, fixture, spec, mint, client, 800 * CKB)
            charged = client.call("broadcast_tx_commit", tx=payment)
            assert int(charged["height"]) <= 10
            rewards = {"mint": mint, "payment": charged}
        if args.replace:
            retired = scenario.change("request-live-exit", 2, public(old), mk_record(public(old), low, 0, 1, old_meta))
        original = snapshot(1, old, genesis[0]["registry"])
        lag_start = progress(observer, 6, 0)
        processes[public(observer)].kill()
        processes[public(observer)].wait(timeout=8)
        joined = [progress(key, 11, 1) for key in (old, incoming)]
        increased = None
        stake_client = Client(args, ports[public(incoming)], hex_bytes(scenario.network))
        archive = args.output / f"node-{ports[public(incoming)]}" / "db"
        relay = Relay(args, l1, fixture, scenario.checkpoint, archive, spec, genesis_committee, stake_client) if args.publish_checkpoint or args.exercise_publisher or args.checkpoint_fee_fund else None
        if delegation:
            delegation["activation"] = confirm_positions(args, scenario, relay, public(old), genesis[0]["registry"], original)
        if rewards:
            rewards["quotes"] = reward_quotes(args, scenario, stake_client, verify, genesis[0], genesis_committee,
                old, old_meta, genesis_positions)
        if args.partial_reduction:
            key = public(old)
            failed = scenario.delegation("unauthorized-reduction", 12, key, 65 * CKB,
                expected="error code 23", funded_by_owner=False)
            holder = scenario.delegation("request-partial-reduction", 12, key, 65 * CKB)
            second = scenario.delegation("request-full-reduction", 12, key, 70 * CKB, label="second")
            missing = scenario.delegation("withdraw-before-confirmation", 14, key, since=0,
                expected="error code 23", funded_by_owner=False)
            reductions = {"sequence": scenario.sequence, "holder": holder, "second": second,
                          "unauthorized_rejected": failed, "unconfirmed_rejected": missing}
            if args.reduction_reorg:
                l1.mine(2)
            second_applied = snapshot(2, incoming, original)
            active = [progress(key, 21, 2) for key in (old, incoming)]
            result = exercise_reductions(args, l1, scenario, relay, old, incoming, second_applied, reductions, active)
            if not args.reduction_slash and not args.reduction_reorg:
                third_applied = snapshot(3, incoming, second_applied)
                after_switch = [progress(key, 31, 3) for key in (old, incoming)]
                assert [status["voting_power"] for status in after_switch] == [360, 408]
                publish_handoff(args, scenario, relay, 2)
                application, proof_height = registry_application(stake_client, scenario.registry_id,
                    third_applied["sequence"])
                assert application["effective_height"] == 31
                checkpoint = relay.advance(proof_height, scenario.secp)
                activation = scenario.delegation("activate-switched-validator", 16, public(incoming),
                    label="redelegated", application=application, checkpoint=checkpoint, funded_by_owner=False)
                result["redelegated_activation"] = {"position": activation, "application": application,
                    "committee": after_switch}
                result["checkpoint"] = relay.report()
            common = min(request(key, STATUS)["height"] for key in (old, incoming))
            agreed = header(incoming, common)
            assert header(old, common) == agreed
            return {"initial": initial, "joined": joined, "agreed": agreed, "reductions": result, "delegation": delegation}
        active_freeze = None
        early = None
        if args.retire and args.replace:
            early, marker_at = retirement_marker(stake_client, public(old), 11)
            assert bytes.fromhex(header(incoming, marker_at)["header"])[112:144] == bytes.fromhex(header(incoming, 10)["header"])[112:144]
        if args.replace:
            assert [status["member"] for status in joined] == [None, 0]
            assert [status["voting_power"] for status in joined] == [None, 400]
            increased = scenario.change("increase-incoming", 1, public(incoming),
                mk_record(public(incoming), 450 * CKB, 1, None, new_meta))
        else:
            assert sorted(status["member"] for status in joined) == [0, 1]
            old_power = 495 if delegation else 250
            expected = [442, 357] if args.delegation_power_cap else [old_power, 400]
            assert [status["voting_power"] for status in joined] == expected
            if args.active_slash:
                if not relay.publications:
                    publish_handoff(args, scenario, relay, 0)
                target = progress(incoming, 13, 1)["height"]
                active_dep = relay.advance(target, scenario.secp)
                active_freeze = freeze_fixture(args, scenario, relay, old, 1, 12, target, old_meta, low, None, active_dep)
                retired = None
            else:
                retired = scenario.change("request-live-exit", 2, public(old), mk_record(public(old), low, 0, 2, old_meta))
        snapshot(2, incoming, original)
        current = [progress(key, 21, 2) for key in (old, incoming)]
        assert current[0]["member"] is None and current[1]["member"] == 0
        assert [status["voting_power"] for status in current] == [None, 450 if args.replace else 400]
        if rewards:
            raw = request(incoming, HANDOFF, le(1, 8))["proof"]
            verify(raw, "handoff-1", (0,), terminal=True)
            next_epoch, checked = reward_epoch(stake_client, verify, 1)
            assert int(next_epoch["budget"], 16) == 0
            rewards["next_epoch"] = next_epoch
            rewards["next_epoch_authenticated_header"] = checked
        if args.retire and not args.replace:
            early, marker_at = retirement_marker(stake_client, public(old), 21)
            assert bytes.fromhex(header(incoming, marker_at)["header"])[112:144] == bytes.fromhex(header(incoming, 20)["header"])[112:144]

        # The old process is now outside the certified roster. Restart it on the handed-off journal.
        processes[public(old)].kill()
        processes[public(old)].wait(timeout=8)
        while_old_offline = progress(incoming, 24 + args.observer_extra_lag, 2,
                                     seconds=65 + args.observer_extra_lag * cadence / 1000)
        write_json(args.output / "before-restart.json", {"observer": lag_start, "incoming": while_old_offline})
        restart_started = time.perf_counter()
        launch(old, False)
        launch(observer, False)
        caught_up = [progress(key, 25 + args.observer_extra_lag, 2) for key in (old, incoming, observer)]
        recovery_seconds = time.perf_counter() - restart_started
        if args.observer_extra_lag > 192:
            assert caught_up[0]["replayed_blocks"] > 0 and caught_up[2]["replayed_blocks"] > 0, caught_up
        common_height = min(status["height"] for status in caught_up)
        agreed = header(incoming, common_height)
        assert all(header(key, common_height) == agreed for key in keys)
        write_json(args.output / "catchup-verified.json", {"replicas": caught_up, "header": agreed,
                                                          "restart_to_visibility_seconds": recovery_seconds})

        handoffs = []
        for outgoing in (0, 1):
            raw = request(observer, HANDOFF, struct.pack("<Q", outgoing))["proof"]
            authenticated = verify(raw, f"handoff-{outgoing}", range(outgoing), terminal=True)
            assert authenticated == header(incoming, (outgoing + 1) * 10)
            handoffs.append(authenticated)
        ordinary = request(observer, FINALITY, struct.pack("<Q", common_height))
        assert ordinary["epoch"] == 2 and not ordinary["handoff"]
        verified = verify(ordinary["proof"], "post-handoff", (0, 1))
        assert verified["height"] >= common_height and verified == header(incoming, verified["height"])
        wrong_epoch = subprocess.run([args.binary, "jolteon-proof", "--genesis", spec,
            "--proof", args.output / "post-handoff.json", "--handoff", args.output / "handoff-0.json"],
            text=True, capture_output=True)
        assert wrong_epoch.returncode != 0
        tampered = bytearray.fromhex(json.loads((args.output / "handoff-0.json").read_text()))
        tampered[-1] ^= 1
        corrupted_path = args.output / "invalid-handoff.json"
        write_json(corrupted_path, tampered.hex())
        bad_handoff = subprocess.run([args.binary, "jolteon-proof", "--genesis", spec,
            "--proof", corrupted_path, "--terminal"], text=True, capture_output=True)
        assert bad_handoff.returncode != 0
        checkpoints = exit_report = None
        if args.publish_checkpoint:
            exit_case = (old, old_meta, low, 11 if args.replace else 21, early) if args.retire else None
            checkpoints, exit_report = publish_checkpoints(args, l1, scenario, relay, spec,
                stake_client, verified["height"], exit_case, active_freeze)
        if args.exercise_publisher:
            checkpoints = exercise_publisher(args, l1, fixture, relay, scenario.secp, common_height)
        if args.checkpoint_fee_fund:
            checkpoints = funded_handoff(args, l1, fixture, scenario, relay)
        if rewards:
            def restart_archive():
                before = request(incoming, STATUS)
                processes[public(incoming)].kill()
                processes[public(incoming)].wait(timeout=8)
                started = time.perf_counter()
                launch(incoming, False)
                after = progress(incoming, before["height"], before["epoch"])
                return {"before": before, "after": after, "restart_seconds": time.perf_counter() - started}

            rewards["payments"] = reward_payouts(args, l1, fixture, vault, network_spec, spec, lane,
                stake_client, relay, verify, restart_archive, rewards)
            paid_height = max(int(item["payout"]["settlement"]["height"]) for item in rewards["payments"]["payouts"])
            for key in keys:
                progress(key, paid_height, 2)
            rewards["payments"]["agreed"] = header(incoming, paid_height)
            assert all(header(key, paid_height) == rewards["payments"]["agreed"] for key in keys)
        restored = None
        if args.snapshot_restore:
            proc = processes[public(observer)]
            proc.terminate()
            proc.wait(timeout=8)
            snapshot = args.output / "observer.snapshot"
            certificate = args.output / "observer-certificate.json"
            export = json.loads(run(args.binary, "jolteon-snapshot", "export",
                "--home", args.output / f"node-{ports[public(observer)]}", "--genesis", spec,
                "--snapshot", snapshot, "--certificate", certificate))
            height = export["height"]
            relay.advance(height, scenario.secp)
            l1.mine(4)
            live = l1.call("get_live_cell", relay.point, True)
            assert live["status"] == "live" and live["cell"]["output"]["type"] == relay.type, live
            raw = binary(live["cell"]["data"]["content"])[64:]
            trusted = hex_bytes(ckb_hash(b"Stem Header\0" + scenario.network + raw))
            assert export["header_id"] == trusted[2:], export
            home = args.output / "restored-observer"
            restore = [args.binary, "jolteon-snapshot", "restore", "--home", home, "--genesis", spec,
                       "--history-window", "64",
                       "--snapshot", snapshot, "--certificate", certificate, "--key-file", key_files[public(observer)]]
            wrong = subprocess.run([*restore, "--trusted-header", "11" * 32], capture_output=True, text=True)
            assert wrong.returncode != 0 and "pinned finalized Header" in wrong.stderr and not home.exists(), wrong
            damaged = bytearray(snapshot.read_bytes())
            damaged[-1] ^= 1
            altered = args.output / "altered-observer.snapshot"
            altered.write_bytes(damaged)
            broken = restore.copy()
            broken[broken.index(snapshot)] = altered
            rejected = subprocess.run([*broken, "--trusted-header", trusted], capture_output=True, text=True)
            assert rejected.returncode != 0 and not home.exists(), rejected
            result = json.loads(run(*restore, "--trusted-header", trusted))
            assert result["observer_only"] and result["height"] == height, result
            launch(observer, False, home)
            after = progress(observer, height + 2, 2)
            assert after["observer_only"] and after["member"] is None, after
            progress(incoming, height + 2, 2)
            hashes = [header(key, height + 2) for key in (observer, incoming)]
            write_json(args.output / "snapshot-headers.json", hashes)
            assert "header" in hashes[0] and hashes[0] == hashes[1], hashes
            restored = {"export": export, "ckb_live": live, "ckb_out_point": relay.point,
                        "ckb_inclusion": l1.prove_committed(relay.point["tx_hash"]),
                        "restored": result, "after": after, "rejected_pin": wrong.stderr,
                        "rejected_state": rejected.stderr, "snapshot_bytes": snapshot.stat().st_size}
        return {
            "replace": args.replace, "l1_transactions": {"registered_old": registered_old, "activated_old": activated_old,
                                "registered_incoming": registered_new, "activated_incoming": activated_new,
                                "exit_requested": retired, "increased_incoming": increased},
            "initial": initial, "observer_stopped": lag_start, "joined": joined, "committee_two": current,
            "after_retired_offline": while_old_offline, "caught_up": caught_up, "agreed": agreed,
            "restart_to_visibility_seconds": recovery_seconds,
            "handoffs": handoffs, "post_handoff_header": verified, "missing_handoff_rejected": wrong_epoch.stderr,
            "altered_handoff_rejected": bad_handoff.stderr,
            "ckb_checkpoints": checkpoints, "ckb_retirement": exit_report, "snapshot_restore": restored,
            "delegation": delegation, "rewards": rewards, "checkpoint_policy": checkpoint_policy,
        }
    finally:
        for child in processes.values():
            if child.poll() is None:
                child.terminate()
        for child in processes.values():
            try:
                child.wait(timeout=8)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("binary", "l1", "stake-registry", "direct-checkpoint", "output"):
        parser.add_argument(f"--{arg}", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--port", type=int, default=22100)
    parser.add_argument("--l1-port", type=int, default=22110)
    parser.add_argument("--observer-extra-lag", type=int, default=0, help="additional blocks before restarting both offline replicas")
    parser.add_argument("--snapshot-restore", action="store_true", help="rebuild a fresh observer from a confirmed CKB checkpoint")
    parser.add_argument("--replace", action="store_true", help="replace the entire first committee, then increase power")
    parser.add_argument("--publish-checkpoint", action="store_true", help="publish native Jolteon checkpoints on the CKB devnet")
    parser.add_argument("--exercise-publisher", action="store_true", help="exercise durable sparse publication on a disposable CKB devnet")
    parser.add_argument("--checkpoint-fee-fund", type=lambda path: Path(path).resolve(),
                        help="fund the first CKB committee handoff from a restricted fee pot")
    parser.add_argument("--retire", action="store_true", help="retire and unbond the outgoing stake using the native checkpoint")
    parser.add_argument("--slash", action="store_true", help="slash the outgoing fixture validator using native vote evidence")
    parser.add_argument("--active-slash", action="store_true", help="freeze while the outgoing validator is still active")
    parser.add_argument("--delegation", action="store_true", help="fund and settle two separate CKB delegation positions")
    parser.add_argument("--delegation-power-cap", action="store_true", help="exercise a committee cap below raw delegated power")
    parser.add_argument("--partial-reduction", action="store_true", help="exit full and partial positions while the validator stays active")
    parser.add_argument("--reduction-slash", action="store_true", help="slash a pending reduction with a historical vote proof")
    parser.add_argument("--reduction-reorg", action="store_true", help="replace the CKB snapshot anchor after handoff")
    parser.add_argument("--reward-accounting", action="store_true", help="bridge CKB and quote closed epoch rewards")
    for optional in ("vault", "deposit-lock", "native-lock", "wrapped"):
        parser.add_argument(f"--{optional}", type=lambda path: Path(path).resolve())
    args = parser.parse_args()
    if args.checkpoint_fee_fund and (args.exercise_publisher or args.publish_checkpoint or args.delegation or args.slash):
        parser.error("--checkpoint-fee-fund uses an isolated committee handoff fixture")
    if args.exercise_publisher and (args.publish_checkpoint or args.delegation or args.slash):
        parser.error("--exercise-publisher uses its own isolated checkpoint and CKB reorg scenario")
    if args.observer_extra_lag < 0:
        parser.error("--observer-extra-lag must be nonnegative")
    if args.active_slash and (not args.slash or args.replace):
        parser.error("--active-slash exercises the join fixture with --slash")
    if args.retire and not args.publish_checkpoint:
        parser.error("--retire requires --publish-checkpoint")
    if args.snapshot_restore and not args.publish_checkpoint:
        parser.error("--snapshot-restore requires --publish-checkpoint")
    if args.slash and not args.retire:
        parser.error("--slash requires --retire")
    if args.delegation and (not (args.retire or args.partial_reduction) or args.replace):
        parser.error("--delegation uses the committee join and retirement scenario")
    if args.partial_reduction and (not args.delegation or not args.publish_checkpoint or args.delegation_power_cap or args.retire):
        parser.error("--partial-reduction uses --delegation --publish-checkpoint with an active validator")
    if args.reduction_slash and not args.partial_reduction:
        parser.error("--reduction-slash requires --partial-reduction")
    if args.reduction_reorg and (not args.partial_reduction or args.reduction_slash):
        parser.error("--reduction-reorg uses its own --partial-reduction run")
    if args.delegation_power_cap and not args.delegation:
        parser.error("--delegation-power-cap requires --delegation")
    if args.reward_accounting and (not args.delegation or not args.retire or
            not all((args.vault, args.deposit_lock, args.native_lock, args.wrapped))):
        parser.error("--reward-accounting needs the delegation retirement fixture and bridge contract binaries")
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    try:
        fixture = Fixtures(args)
        report = run_epochs(args, l1, Scenario(args, l1, fixture), fixture)
        write_json(args.output / "jolteon-stake-report.json", report)
        if args.reduction_reorg:
            print(f"Rejected an orphaned and substituted CKB anchor at Stem height {report['agreed']['height']}")
        elif args.partial_reduction:
            print(f"Verified in-term CKB delegation settlements and agreed height {report['agreed']['height']}")
        else:
            print(f"Verified two CKB-backed Jolteon handoffs, with all replicas at height {report['agreed']['height']}")
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

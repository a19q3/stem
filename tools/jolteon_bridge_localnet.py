#!/usr/bin/env python3
"""Exercise the native checkpoint and permissionless CKB and xUDT payouts."""

import argparse
import base64
import copy
import json
from pathlib import Path
import secrets
import struct
import subprocess

from bridge_localnet import (CKB, FEE, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes, l1_reference_request,
                             le, point, reject_cli, script, write_key)
from jolteon_checkpoint_localnet import PublisherFixture, Relay
from intercell_localnet import intercell_round
from security_domain_localnet import Scenario, use_stem_receipt
from checkpoint_fee_fund_localnet import deploy_fee_lock, use_fee_fund
from asset_export_localnet import asset_round
from metatype_multiasset_localnet import multiasset_round
from spore_settlement_localnet import spore_round
from object_export_localnet import object_round
from jolteon_localnet import FINALITY, HANDOFF, HEADER, QUERY, STATUS, SUBMIT, evidence_keys, rpc, write_configuration
from localnet import run, write_json
from native_bridge_localnet import (bridge_configuration, cell, deploy, deploy_token, deposit_and_mint, lane_proof,
                                    reconcile_backing, redeem_withdrawal, signing_cell, start, stem_id,
                                    vault_operations, verify_l1_output, wait_for, withdraw_and_settle, xudt_round_trip)

BLOCK_CYCLES = 400_000_000


class Client:
    """Adapter for the shared bridge fixture, using only finalized native queries."""

    def __init__(self, args, port, network):
        self.args, self.port, self.network = args, port, network
        self.locations = {}

    def status(self):
        return rpc(self.port, STATUS)

    def header(self, height):
        return rpc(self.port, HEADER, struct.pack("<Q", height))

    def finality(self, height):
        return rpc(self.port, FINALITY, struct.pack("<Q", height))

    def handoff(self, epoch):
        return rpc(self.port, HANDOFF, struct.pack("<Q", epoch))

    def query(self, path, *, data="0x", height=0, prove=False):
        query = json.dumps(dict(path=path, data=data, height=int(height), prove=prove)).encode()
        reply = rpc(self.port, QUERY, query)
        return reply

    def call(self, method, **params):
        if method == "status":
            return {"sync_info": {"latest_block_height": self.status()["height"]}}
        if method == "abci_query":
            path = json.loads(params["path"])
            reply = self.query(path, data=params.get("data", "0x"),
                               height=params.get("height", 0), prove=params.get("prove") == "true")
            if "error" in reply:
                return {"response": {"code": 1, "log": reply["error"]}}
            return {"response": {"code": 0, "height": reply["height"],
                    "value": base64.b64encode(bytes.fromhex(reply["value"])).decode("ascii")}}
        if method == "tx":
            return self.locations[params["hash"].removeprefix("0x")]
        if method not in ("broadcast_tx_sync", "broadcast_tx_commit"):
            raise ValueError(f"unsupported bridge fixture request {method}")
        first_height = self.status()["height"] + 1
        admitted = rpc(self.port, SUBMIT, binary(params["tx"]))
        if "error" in admitted:
            return {"code": 1, "log": admitted["error"]}
        if method == "broadcast_tx_sync":
            return {"code": 0, "hash": admitted["admitted"]}
        id_ = stem_id(self.args, self.network, params["tx"]).removeprefix("0x")
        next_height = first_height

        def committed():
            nonlocal next_height
            latest = self.status()["height"]
            while next_height <= latest:
                result = self.query("/stem/transactions", height=next_height)
                ids = json.loads(bytes.fromhex(result["value"]))
                if id_ in ids:
                    self.locations[id_] = {"index": ids.index(id_), "height": next_height}
                    return next_height
                next_height += 1
            return None

        height = wait_for(committed, "Jolteon finalized bridge transaction", seconds=35)
        return {"height": height, "hash": id_, "check_tx": {"code": 0}}


def fee_payment(args, fixture, spec_path, mint, client, fee):
    native_lock = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())),
                         fixture.key(fixture.owner)["blake160"], "data")
    request, draft, context, lock = (args.output / f"fee-payment-{name}.json"
                                     for name in ("request", "draft", "context", "lock"))
    write_json(request, {"inputs": [{"out_point": mint["payment"]}],
                         "outputs": [cell(mint["payment_capacity"] - fee, native_lock)],
                         "cell_deps": [dependency(mint["lock_dep"])]})
    write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request)))
    write_json(context, {"cells": [signing_cell(client, mint[key]) for key in ("payment", "lock_dep")]})
    write_json(lock, native_lock)
    payment = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
                             "--transaction", draft, "--context", context, "--group-index", "0",
                             "--lock", lock, "--key-file", fixture.owner))

    return payment


def fee_round(args, l1, fixture, vault, spec, spec_path, lane, mint, client, relay, fee_fund):
    fee = (8000 if args.expense_grant else 4000 if fee_fund else 800) * CKB
    payment = fee_payment(args, fixture, spec_path, mint, client, fee)

    def proved(path):
        reply = client.query(path, prove=True)
        return json.loads(bytes.fromhex(reply["value"]))

    def claim(fund, amount, claimed_before=0):
        return json.loads(run(args.binary, "bridge", "claim-fees", "--fund", str(fund),
                              "--amount", str(amount), "--claimed-before", str(claimed_before)))

    def reject(tx, message):
        result = client.call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and message in result["log"], result
        return result["log"]

    unearned = reject(claim(0, 100 * CKB), "exceeds fund allocation")
    zero_revenue = vault_revenue_proof(args, client, "before-fee", {lane: 0})
    paid_fee = client.call("broadcast_tx_commit", tx=payment)
    before = proved("/stem/fee-funds")
    assert int(before["extinguished_shannons"], 16) == fee, before
    attributed = vault_revenue_proof(args, client, "before-fund-claims", {lane: fee})
    amounts = [fee * basis // 10_000 for basis in (6500, 1500, 1000, 1000)]
    wrong_amount = reject(claim(0, amounts[0] + 1), "exceeds fund allocation")
    dust = reject(claim(0, 1), "below L1 occupied Capacity")
    debits = [(fund, amount, 0) for fund, amount in enumerate(amounts)]
    if fee_fund:
        half = amounts[3] // 2
        debits[3:] = [(3, half, 0), (3, amounts[3] - half, half)]
    claims = []
    for fund, amount, claimed in debits:
        tx = claim(fund, amount, claimed)
        claims.append(client.call("broadcast_tx_commit", tx=tx))
    replay = reject(claim(0, amounts[0]), "stale fee claim")
    queued = proved("/stem/fee-funds")
    backing_queued = proved("/stem/bridge-backing")
    assert int(backing_queued["pending_shannons"], 16) == fee
    assert int(backing_queued["unallocated_shannons"], 16) == 0
    assert [int(fund["queued_shannons"], 16) for fund in queued["accounts"]] == amounts
    assert [int(fund["paid_shannons"], 16) for fund in queued["accounts"]] == [0] * 4

    vault_input = point(mint["l1_consolidation"], 0)
    fee_input = point(mint["l1_consolidation"], 1)
    previous_anchor = mint
    payouts = []
    for nonce, ((fund, amount, _), transaction) in enumerate(zip(debits, claims)):
        recipient = spec["bridge"]["fee_funds"][fund]["recipient"]
        redeemed = redeem_withdrawal(
            args, l1, fixture, vault, spec, spec_path, lane, client, relay,
            name=f"fee-fund-{nonce}", burned=transaction, recipient=recipient, capacity=amount,
            units=b"", nonce=nonce, input_vault=vault_input, fee_input=fee_input, previous_anchor=previous_anchor,
        )
        output = l1.call("get_transaction", redeemed["l1_payout"])["transaction"]["outputs"][0]
        assert output["lock"] == recipient and int(output["capacity"], 16) == amount
        payouts.append(redeemed)
        vault_input = point(redeemed["l1_payout"], 1)
        fee_input = point(redeemed["l1_payout"], 2)
        previous_anchor = redeemed
    after = proved("/stem/fee-funds")
    assert [int(fund["paid_shannons"], 16) for fund in after["accounts"]] == amounts
    backing = proved("/stem/bridge-backing")
    assert int(backing["liquid_shannons"], 16) == int(backing["circulating_shannons"], 16) == mint["capacity"] - fee
    assert int(backing["pending_shannons"], 16) == int(backing["unallocated_shannons"], 16) == 0
    expenses = use_fee_fund(args, l1, fixture, vault, relay, fee_fund, payouts[-2:], fee_input) if fee_fund else None
    assert proved("/stem/bridge-backing")["record"] == backing["record"]
    attributed_after = vault_revenue_proof(args, client, "after-fund-payouts", {lane: fee}, negative=True)
    assert attributed_after["query"]["record"] == attributed["query"]["record"]
    operating = None
    if args.expense_grant:
        index = {"devfund": 1, "ecosystem": 2, "pow": 3}[args.expense_account]
        source = point(payouts[index]["l1_payout"], 0)
        provider = spec["bridge"]["fee_funds"][index]["recipient"]
        scenario = Scenario(args, l1, fixture, args.expense_service, funding=vault["change_point"],
            network=spec["network_id"], grant_funding=(source, provider, args.output / f"fund-{index}.key"))
        scenario.open_domain()
        scenario.open_request("query-one")
        reconciled = scenario.committed["query-one"]["reimbursement"]
        assert reconciled["treasury_input"] == source and reconciled["authorization"]["tx"]["inputs"][0]["previous_output"] == source
        operating = {"fund_index": index, "fund_name": args.expense_account, "paid_shannons": amounts[index],
                     "payout": payouts[index], "service_request": scenario.committed["query-one"], "reimbursement": reconciled}
    return {"vault_revenue": {"before_fee": zero_revenue, "before_claims": attributed,
                              "after_payouts": attributed_after},
            "payment": paid_fee, "before": before, "queued": queued, "backing_queued": backing_queued,
            "payouts": payouts, "after": after, "backing": backing, "checkpoint_expenses": expenses, "operating_expenses": operating,
            "rejections": {"unearned": unearned, "overclaim": wrong_amount, "dust": dust, "replay": replay}}


def reference_round(args, l1, fixture, spec, spec_path, lane, mint, client):
    def query(path, data="0x"):
        reply = client.query(path, data=data, prove=True)
        return json.loads(bytes.fromhex(reply["value"]))

    anchor_query = client.query("/stem/bridge-anchor", prove=True)
    anchor = json.loads(bytes.fromhex(anchor_query["value"]))
    assert anchor["height"] == mint["confirmed_height"] and anchor["hash"] == mint["confirmed_hash"]
    l1.mine(spec["bridge"]["confirmations"])
    live_point = point(mint["l1_consolidation"], 0)
    spent_point = point(mint["l1_deposits"], 1)
    historical_inclusion = l1.prove_committed(mint["l1_deposits"])

    def attest(label, target, live, height=anchor["height"], block=anchor["hash"], rejected=None):
        path = args.output / f"reference-{label}-request.json"
        write_json(path, l1_reference_request(height, block, (target, live)))
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            command = [args.binary, "bridge", "attest-reference", "--genesis", spec_path,
                       "--l1-rpc", l1.url, "--request", path, "--key-file", key,
                       "--journal", args.output / f"journal-{index}"]
            if rejected:
                result = subprocess.run(command, text=True, capture_output=True)
                assert result.returncode and rejected in result.stderr, result
                return result.stderr
            votes.append(json.loads(run(*command)))
        assert len({tuple(vote["records"]) for vote in votes}) == 1
        votes_file = args.output / f"reference-{label}-votes.json"
        write_json(votes_file, votes)
        return votes, votes_file

    def reject(tx, message):
        answer = client.call("broadcast_tx_sync", tx=tx)
        assert answer["code"] == 1 and message in answer["log"], answer
        return answer["log"]

    native_lock = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())),
                         fixture.key(fixture.owner)["blake160"], "data")
    script_code = ckb_hash(args.reference_script.read_bytes())
    code_points = [mint["lock_dep"], point(mint["mint_id"], 2)]
    lock_file = args.output / "reference-owner-lock.json"
    write_json(lock_file, native_lock)

    def sign(label, input_point, outputs, votes=None):
        request, unsigned, context = (args.output / f"reference-{label}-{part}.json"
                                      for part in ("build", "unsigned", "context"))
        write_json(request, {"inputs": [{"out_point": input_point}], "outputs": outputs,
                    "cell_deps": [dependency(p) for p in code_points]})
        write_json(unsigned, json.loads(run(args.binary, "wallet", "build", "--request", request)))
        if votes:
            attached = args.output / f"reference-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references",
                "--genesis", spec_path, "--transaction", unsigned, "--observations", votes)))
            unsigned = attached
        write_json(context, {"cells": [signing_cell(client, p) for p in [input_point, *code_points]]})
        encoded = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
            "--transaction", unsigned, "--context", context, "--group-index", "0",
            "--lock", lock_file, "--key-file", fixture.owner))
        write_json(args.output / f"reference-{label}-signed.json", encoded)
        return encoded

    part = mint["payment_capacity"] // 3
    split = sign("split", mint["payment"], [cell(part, native_lock), cell(part, native_lock), cell(part - FEE, native_lock)])
    split_tx = client.call("broadcast_tx_commit", tx=split)
    assert split_tx["check_tx"]["code"] == 0, split_tx
    live_input, history_input, refresh_input = [point("0x" + split_tx["hash"], index) for index in range(3)]
    live_votes, live_file = attest("live", live_point, True)
    old_votes, old_file = attest("historical", spent_point, False)
    historical_live_votes, historical_live_file = attest("historical-vault", live_point, False)
    errors = {"spent_observation": attest("spent", spent_point, True, rejected="no longer live")}

    def output(record, kind, capacity):
        policy = script(hex_bytes(script_code), hex_bytes(bytes([kind]) + ckb_hash(binary(record))), "data")
        return [cell(capacity, native_lock, types=[policy])]

    live_outputs = output(live_votes[0]["records"][0], 1, part - FEE)
    errors["missing"] = reject(sign("missing", live_input, live_outputs), "exited with 13")
    errors["wrong_kind"] = reject(sign("wrong-kind", live_input,
        output(historical_live_votes[0]["records"][0], 1, part - FEE), historical_live_file), "exited with 13")
    submit_live = sign("accepted-live", live_input, live_outputs, live_file)
    corrupted = bytearray(binary(submit_live))
    signature = binary(live_votes[0]["signature"])
    offset = corrupted.index(signature)
    corrupted[offset] ^= 1
    errors["invalid_signature"] = reject(hex_bytes(corrupted), "Signature")
    live_tx = client.call("broadcast_tx_commit", tx=submit_live)
    assert live_tx["check_tx"]["code"] == 0, live_tx

    l1.mine(2)
    newer = anchor["height"] + 1
    future_hash = l1.call("get_block_hash", hex(newer))
    future_votes, future_file = attest("other-anchor", live_point, True, newer, future_hash)
    history_outputs = output(old_votes[0]["records"][0], 0, part - FEE)
    errors["uncommitted_anchor"] = reject(sign("uncommitted-anchor", history_input, history_outputs, future_file),
                                          "committed CKB anchor")
    expiry = anchor["adopted_at"] + spec["limits"]["header_dep_age"]
    wait_for(lambda: client.status()["height"] >= expiry, "expire the CKB live observation", seconds=65)
    errors["stale_live"] = reject(sign("stale-live", history_input, live_outputs, live_file), "observation is stale")
    submit_history = sign("accepted-history", history_input, history_outputs, old_file)
    history = client.call("broadcast_tx_commit", tx=submit_history)
    assert history["check_tx"]["code"] == 0, history

    def attest_anchor(height, predecessor, rejected=None):
        values = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            command = [args.binary, "bridge", "attest-anchor", "--genesis", spec_path,
                       "--l1-rpc", l1.url, "--previous-height", str(predecessor["height"]),
                       "--previous-hash", predecessor["hash"], "--height", str(height),
                       "--journal", args.output / f"journal-{index}", "--key-file", key]
            if rejected:
                result = subprocess.run(command, text=True, capture_output=True)
                assert result.returncode and rejected in result.stderr, result
                return result.stderr
            values.append(json.loads(run(*command)))
        return values

    def assemble_anchor(label, values):
        path = args.output / f"anchor-{label}-votes.json"
        write_json(path, values)
        tx = json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", path))
        write_json(args.output / f"anchor-{label}-transaction.json", tx)
        return tx

    errors["unconfirmed_refresh"] = attest_anchor(int(l1.call("get_tip_block_number"), 16), anchor,
                                                  rejected="insufficient confirmations")
    approvals = attest_anchor(newer, anchor)
    errors["minority_refresh"] = reject(assemble_anchor("minority", approvals[:2]), "Quorum")
    by_signer = {vote["signer"]: vote["signature"] for vote in future_votes}
    wrong_domain = [dict(vote, signature=by_signer[vote["signer"]]) for vote in approvals]
    errors["reference_signature_replay"] = reject(assemble_anchor("wrong-domain", wrong_domain), "Signature")
    update = assemble_anchor("accepted", approvals)
    backing_before = query("/stem/bridge-backing")
    lane_before = query("/stem/bridge-lane", lane)
    fees_before = query("/stem/extinguished-fees")
    advanced = client.call("broadcast_tx_commit", tx=update)
    assert advanced["check_tx"]["code"] == 0, advanced
    fresh = query("/stem/bridge-anchor")
    assert (fresh["height"], fresh["hash"], fresh["adopted_at"]) == (newer, future_hash, advanced["height"])
    errors["refresh_replay"] = reject(update, "does not extend the accepted anchor")
    backing_after = query("/stem/bridge-backing")
    lane_after = query("/stem/bridge-lane", lane)
    fees_after = query("/stem/extinguished-fees")
    assert backing_before["record"] == backing_after["record"]
    assert lane_before["record"] == lane_after["record"]
    assert fees_before["extinguished_shannons"] == fees_after["extinguished_shannons"]
    errors["superseded_anchor"] = reject(
        sign("superseded", refresh_input, output(old_votes[0]["records"][0], 0, part - 2 * FEE), old_file),
        "committed CKB anchor")
    refreshed_tx = sign("refreshed-live", refresh_input,
        output(future_votes[0]["records"][0], 1, part - 2 * FEE), future_file)
    refreshed = client.call("broadcast_tx_commit", tx=refreshed_tx)
    assert refreshed["check_tx"]["code"] == 0, refreshed

    def execution(tx):
        location = client.locations[tx["hash"]]
        response = client.query("/stem/execution", data=hex_bytes(le(location["index"], 4)),
                                height=location["height"], prove=True)
        evidence = json.loads(bytes.fromhex(response["value"]))
        raw = binary(evidence["receipt"])
        compact = iter(raw[1:])

        def number():
            value = 0
            for shift in range(0, 70, 7):
                digit = next(compact)
                value |= (digit & 127) << shift
                if digit < 128:
                    return value
            raise AssertionError("oversized receipt number")

        fee, cycles = number(), number()
        return {"fee": fee, "cycles": cycles, "proof": evidence}

    ancestor = l1.call("get_block_hash", hex(anchor["height"] - 1))
    l1.call("truncate", ancestor)
    l1.mine(spec["bridge"]["confirmations"] + 3)
    replacement = l1.call("get_block_hash", hex(anchor["height"]))
    assert replacement != anchor["hash"]
    errors["reorganized_observation"] = attest("reorganized", spent_point, False, rejected="anchor was reorganized")
    errors["reorganized_refresh"] = attest_anchor(newer + 1, fresh, rejected="anchor was reorganized")
    executions = [execution(tx) for tx in (live_tx, history, refreshed)]
    assert all(result["cycles"] >= 12_000_000 for result in executions)
    update_receipt = execution(advanced)
    assert update_receipt["fee"] == update_receipt["cycles"] == 0
    return {"anchor": anchor, "live": live_tx, "historical": history, "split": split_tx,
            "live_execution": executions[0], "historical_execution": executions[1], "refreshed_execution": executions[2],
            "accepted_transaction_bytes": [len(binary(tx)) for tx in (submit_live, submit_history, refreshed_tx)],
            "refresh": {"anchor": fresh, "transaction": advanced, "bytes": len(binary(update)), "execution": update_receipt,
                        "live": refreshed, "backing_before": backing_before, "backing_after": backing_after,
                        "lane_before": lane_before, "lane_after": lane_after,
                        "fees_before": fees_before, "fees_after": fees_after},
            "expiry": expiry, "replacement_anchor": replacement, "rejections": errors,
            "historical_out_point": spent_point, "historical_inclusion": historical_inclusion}


def vault_revenue_proof(args, client, name, expected, *, negative=False):
    reply = client.query("/stem/vault-revenue", prove=True)
    height = reply["height"]
    query = json.loads(bytes.fromhex(reply["value"]))
    header = client.header(height)
    finality = client.finality(height)
    header_file = args.output / f"vault-revenue-{name}-header.json"
    accounting_file = args.output / f"vault-revenue-{name}.json"
    write_json(header_file, "0x" + header["header"])
    write_json(accounting_file, query)
    command = [args.binary, "bridge", "verify-vault-revenue", "--network", client.network,
               "--header", header_file, "--accounting", accounting_file]
    verified = json.loads(run(*command))
    assert verified["height"] == height
    assert {entry["lane"]: int(entry["attributed_shannons"], 16)
            for entry in verified["entries"]} == expected, verified
    assert int(verified["attributed_shannons"], 16) == sum(expected.values())
    rejected = {}
    if negative:
        for kind in ("different-total", "different-network"):
            invalid_command = command.copy()
            if kind == "different-total":
                path = args.output / f"vault-revenue-{name}-{kind}.json"
                write_json(path, dict(query, extinguished_shannons=hex(sum(expected.values()) + 1)))
                invalid_command[-1] = path
                reason = "invalid extinguished fee proof"
            else:
                invalid_command[invalid_command.index("--network") + 1] = "0x" + "ff" * 32
                reason = "invalid vault revenue proof"
            rejected[kind] = reject_cli(reason, *invalid_command)
    return {"query": query, "height": height, "header": header, "finality": finality,
            "verified": verified, "rejections": rejected, "command": [str(arg) for arg in command]}


def split_vault_fee(fees, weights):
    shares = {}
    total = sum(weights.values())
    prefix = allocated = 0
    for lane, weight in sorted(weights.items()):
        prefix += weight
        share = fees * prefix // total
        shares[lane] = share - allocated
        allocated = share
    return shares


def bridge_round(args, l1, fixture, children):
    network = hex_bytes(ckb_hash(b"Stem native Jolteon bridge fixture\0" + secrets.token_bytes(32)))
    vault = deploy(args, l1, fixture, network)
    token = deploy_token(args, l1, fixture, vault) if args.xudt else None
    bridge, lane = bridge_configuration(args, l1, fixture, vault, token, network)
    if args.intercell_mailbox:
        bridge["intercell_mailbox_code"] = hex_bytes(ckb_hash(args.intercell_mailbox.read_bytes()))
    fee_fund = None
    if args.fee_funds:
        bridge["fee_funds"] = [{"basis_points": share, "recipient": dict(fixture.standard, args=fixture.key(
            write_key(args.output, f"fund-{i}", secrets.token_bytes(32)))["blake160"])}
            for i, share in enumerate((6500, 1500, 1000, 1000))]
        if args.checkpoint_fee_fund:
            fee_fund = deploy_fee_lock(args, l1, fixture, vault)
            bridge["fee_funds"][3]["recipient"] = fee_fund["lock"]
            bridge["checkpoint_fee_fund_code"] = fee_fund["lock"]["code_hash"]
    keys = evidence_keys(args)
    members = [{"ed25519": hex_bytes(key), "power": 1} for key, _ in keys]
    spec = {"chain_id": "stem-jolteon-ckb-local", "network_id": network, "genesis_time_ms": 1_735_689_600_000,
            "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
            "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024,
                       "transaction_cycles": args.stem_transaction_cycles, "block_cycles": BLOCK_CYCLES,
                       "cell_reads": 1024,
                       "cell_read_bytes": 64 * 1024 * 1024, "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
            "development_validators": members, "bridge": bridge}
    spec_path = args.output / "stem.json"
    write_json(spec_path, spec)
    genesis_probes = {}
    if fee_fund:
        def rejected(name, mutated, reason):
            draft = args.output / f"checkpoint-fund-invalid-genesis-{name}.json"
            write_json(draft, mutated)
            attempt = subprocess.run([args.binary, "genesis-hash", "--genesis", draft],
                                     text=True, capture_output=True)
            assert attempt.returncode and reason in attempt.stderr, attempt
            genesis_probes[name] = attempt.stderr

        wrong = copy.deepcopy(spec)
        lock = wrong["bridge"]["fee_funds"][3]["recipient"]
        arguments = bytearray(binary(lock["args"]))
        arguments[32] ^= 1
        lock["args"] = hex_bytes(arguments)
        rejected("foreign-checkpoint", wrong, "must target the native vault checkpoint")
        wrong = copy.deepcopy(spec)
        wrong["bridge"]["recipient_locks"].append({"example": fee_fund["lock"]})
        rejected("ordinary-allowlist", wrong, "cannot be a general withdrawal recipient")
    ports = list(range(args.port, args.port + len(keys)))
    configuration = write_configuration(args.output, [key for key, _ in keys], ports, 300, 1600)
    clients = [Client(args, port, network) for port in ports]
    homes = [args.output / f"node-{port}" for port in ports]
    commands = []
    for i, (_, key) in enumerate(keys):
        command = [args.binary, "run", "--home", homes[i], "--genesis", spec_path, "--configuration",
                   configuration, "--key-file", key, "--initialize"]
        if i == 0:
            command.append("--archive")
        commands.append([argument for argument in command if argument != "--initialize"])
        children.append(start(args, command, f"jolteon-{i}.log"))
    client = clients[0]
    wait_for(lambda: all(peer.status()["height"] >= 2 for peer in clients), "native bridge replicas")
    relay = Relay(args, l1, fixture, vault["anchor"], homes[0] / "db", spec_path, members, client)
    publisher = None
    if args.exercise_publisher:
        publisher = PublisherFixture(args, l1, fixture, relay, vault["secp"])
        publisher.publish("bootstrap-checkpoint", 1)
    else:
        relay.advance(1, vault["secp"], probe=True)
    mint = deposit_and_mint(args, l1, fixture, vault, spec, spec_path, lane, client,
                            payment_capacity=(8500 if args.expense_grant else 4400 if fee_fund else 1200) * CKB)
    write_json(args.output / "native-mint-report.json", mint)
    report = {"mint": mint}
    if args.stem_native_object:
        report["native_object"] = object_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay)
    elif args.stem_spore or args.native_spore_origin:
        report["spore_settlement"] = spore_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay)
    elif args.metatype_multiasset:
        report["metatype_multiasset"] = multiasset_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay)
    elif args.metatype_composed_cell:
        report["metatype_composed_cell"] = multiasset_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay)
    elif args.export_udt:
        report["asset_export"] = asset_round(args, l1, fixture, vault, spec, spec_path,
                                              mint, clients, relay)
    elif args.intercell_mailbox:
        report["intercell"] = intercell_round(args, l1, fixture, vault, spec, spec_path,
                                              mint, clients, relay, commands, children, publisher=publisher)
    elif args.security_domain:
        report["security_domain"] = use_stem_receipt(args, l1, fixture, vault, spec, spec_path, mint, client)
    elif args.reference_script:
        report["references"] = reference_round(args, l1, fixture, spec, spec_path, lane, mint, client)
    elif args.fee_funds:
        report["fee_funds"] = fee_round(args, l1, fixture, vault, spec, spec_path, lane, mint, client, relay, fee_fund)
        if genesis_probes:
            report["fee_funds"]["genesis_rejections"] = genesis_probes
    else:
        payout = withdraw_and_settle(args, l1, fixture, vault, spec, spec_path, lane, mint, client, relay)
        report["withdrawal"] = payout
    if token:
        token["custody_cell"] = vault["custody_cell"]
        report["xudt"] = xudt_round_trip(args, l1, fixture, token, spec, spec_path, mint, payout, client, relay)
        asset_lane = report["xudt"]["lane"]
        weights = {lane: mint["capacity"] - payout["capacity"], asset_lane: report["xudt"]["mint"]["capacity"]}
        expected = split_vault_fee(FEE, weights)
        expected[lane] += FEE
        before = vault_revenue_proof(args, client, "before-reserve-move", expected)
        report["vault_operations"] = vault_operations(args, l1, fixture, vault, token, lane, payout,
                                                     report["xudt"], spec_path, client)
        after = vault_revenue_proof(args, client, "after-rotation", expected, negative=True)
        assert before["query"]["record"] == after["query"]["record"]
        report["vault_revenue"] = {"second_fee_weights": weights, "before_reserve_move": before,
                                   "after_rotation": after}
    report["checkpoint"] = (publisher or relay).report()
    if not (args.fee_funds or args.reference_script or args.intercell_mailbox):
        report["backing"] = reconcile_backing(client, report)
        report["authenticated_ckb_outputs"] = verify_l1_output(args, l1, payout, token is not None)
    report["lane_proofs"] = {lane: lane_proof(client, lane)}
    if token:
        asset_lane = report["xudt"]["lane"]
        report["lane_proofs"][asset_lane] = lane_proof(client, asset_lane)
    height = client.status()["height"]
    wait_for(lambda: all(peer.status()["height"] >= height for peer in clients), "all bridge replicas finalized")
    header = client.header(height)
    assert all(peer.header(height) == header for peer in clients)
    report["agreed"] = header
    report["replicas"] = [peer.status() for peer in clients]
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "vault", "deposit-lock", "native-lock", "wrapped", "direct-checkpoint", "output"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True)
    for name in ("xudt", "xudt-gate"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    parser.add_argument("--validators", type=int, default=4)
    parser.add_argument("--stem-transaction-cycles", type=int, default=20_000_000,
                        help="explicit per-transaction cycle limit in the Stem development genesis")
    parser.add_argument("--fee-funds", action="store_true")
    parser.add_argument("--expense-grant", type=lambda path: Path(path).resolve())
    parser.add_argument("--expense-service", type=lambda path: Path(path).resolve())
    parser.add_argument("--expense-account", choices=["devfund", "ecosystem", "pow"])

    parser.add_argument("--native-spore-origin", type=lambda path: Path(path).resolve(),
                        help="issue an unrestricted immutable object on Stem and export to standard Spore V2")
    parser.add_argument("--native-spore-approval", type=lambda path: Path(path).resolve(),
                        help="deploy the native approval Type and carry its policy in a CKB Spore holder Lock")
    native_media = parser.add_mutually_exclusive_group()
    native_media.add_argument("--native-spore-json", action="store_true", help="use application/json data in the native asset fixture")
    native_media.add_argument("--native-spore-multipart", action="store_true", help="use a multipart/mixed native object with a boundary")
    parser.add_argument("--spore-immortal", action="store_true", help="use the published Spore V2 immortal lifecycle")
    parser.add_argument("--checkpoint-fee-fund", type=lambda path: Path(path).resolve(),
                        help="use restricted checkpoint Lock for the PoW Security Fund")
    for name in ("export-udt", "asset-escrow", "ckb-stem-udt"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    for name in ("stem-spore", "ckb-spore-escrow", "spore-v2"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    parser.add_argument("--spore-cluster-v2", type=lambda path: Path(path).resolve(),
                        help="mint the fixture in a Cluster using its published CKB ELF")
    for name in ("stem-native-object", "object-policy", "ckb-stem-object",
                 "stem-metatype-escrow", "metatype-dispatcher", "metatype-mailbox-lock",
                 "metatype-bridged-object", "metatype-bridged-approval", "native-object-state-limit",
                 "metatype-object-state-limit", "metatype-rule-registry", "native-object-append-only",
                 "metatype-object-append-only", "native-object-workflow", "metatype-object-workflow",
                 "native-object-royalty", "metatype-object-royalty",
                 "native-object-rotating-approval", "metatype-object-rotating-approval",
                 "metatype-approval-registry",
                 "native-amount-cap",
                 "metatype-udt-adapter", "metatype-amount-cap-adapter"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    parser.add_argument("--metatype-shared-manifest", action="store_true")
    parser.add_argument("--metatype-multiasset", action="store_true")
    parser.add_argument("--metatype-composed-cell", action="store_true")
    composed_return = parser.add_mutually_exclusive_group()
    composed_return.add_argument("--metatype-composed-partial", action="store_true")
    composed_return.add_argument("--metatype-composed-selective", choices=("partial", "exhaust"))
    parser.add_argument("--metatype-revisable-object", action="store_true")
    parser.add_argument("--metatype-revision-topup", action="store_true")
    parser.add_argument("--metatype-workflow-early-return", action="store_true")
    parser.add_argument("--metatype-object-freeze", action="store_true")
    parser.add_argument("--metatype-approval-quorum", nargs=2, type=int, metavar=("THRESHOLD", "KEYS"))
    parser.add_argument("--metatype-decrease-ceiling", action="store_true")
    parser.add_argument("--full-udt-return", action="store_true")
    parser.add_argument("--finish-udt-return", action="store_true")
    parser.add_argument("--intercell-mailbox", type=lambda path: Path(path).resolve())
    parser.add_argument("--exercise-publisher", action="store_true", help="use the sparse publisher for Intercell checkpoints")
    parser.add_argument("--wasm-fixture", action="store_true", help="export live wallet and proof inputs from the generic Intercell round")
    parser.add_argument("--reference-script", type=lambda path: Path(path).resolve())
    parser.add_argument("--security-domain", type=lambda path: Path(path).resolve(),
                        help="consume a real CKB security receipt through a native L1 reference")
    parser.add_argument("--port", type=int, default=22200)
    parser.add_argument("--l1-port", type=int, default=22210)
    args = parser.parse_args()
    expense = (args.expense_grant, args.expense_service, args.expense_account)
    if any(expense) and (not all(expense) or not args.fee_funds or args.checkpoint_fee_fund):
        parser.error("operating reimbursements require an explicit fee account, grant and service in the ordinary fee fund fixture")
    if not 1 <= args.stem_transaction_cycles <= BLOCK_CYCLES:
        parser.error("--stem-transaction-cycles must be positive and fit in the block budget")
    base = (args.stem_metatype_escrow, args.metatype_dispatcher, args.metatype_mailbox_lock)
    portable_udt = (args.native_amount_cap, args.metatype_udt_adapter, args.metatype_amount_cap_adapter)
    object_adapters = (args.metatype_bridged_object, args.metatype_bridged_approval)
    if any(portable_udt) and (not all((*portable_udt, *base, args.export_udt))
                             or args.asset_escrow or args.ckb_stem_udt or any(object_adapters)):
        parser.error("Metatype UDT settlement needs the shared custody scripts and both UDT adapters")
    if any((args.export_udt, args.asset_escrow, args.ckb_stem_udt)) and (
            not all((args.export_udt, args.intercell_mailbox))
            or not (all(portable_udt) or all((args.asset_escrow, args.ckb_stem_udt)))):
        parser.error("UDT settlement requires --export-udt, an escrow route and --intercell-mailbox")
    if (args.native_spore_json or args.native_spore_multipart) and not args.native_spore_origin:
        parser.error("the native media fixtures require --native-spore-origin")
    if args.native_spore_approval and (not args.native_spore_origin or args.native_spore_json
            or args.native_spore_multipart or args.spore_immortal):
        parser.error("the approved native identity uses the immutable plain Spore content profile")
    if args.spore_immortal and not args.spore_v2:
        parser.error("--spore-immortal requires the original Spore V2 settlement fixture")
    if args.native_spore_origin and any((args.stem_spore, args.spore_cluster_v2, args.stem_native_object, args.export_udt)):
        parser.error("native Spore export requires the unrestricted immutable profile")
    if any((args.native_spore_origin, args.stem_spore, args.ckb_spore_escrow, args.spore_v2, args.spore_cluster_v2)) and (
            not all((args.stem_spore or args.native_spore_origin, args.ckb_spore_escrow, args.spore_v2, args.intercell_mailbox))
            or args.export_udt):
        parser.error("Spore settlement requires its three binaries and the Intercell mailbox")
    metatype_object = all((*base, *object_adapters))
    if (any(object_adapters) and (not metatype_object or args.ckb_stem_object)
            or any(base) and not (metatype_object or all(portable_udt))):
        parser.error("Metatype settlement needs a complete object or fungible route")
    if args.metatype_revisable_object and not metatype_object:
        parser.error("--metatype-revisable-object requires the Metatype object route")
    if args.metatype_revision_topup and not args.metatype_revisable_object:
        parser.error("--metatype-revision-topup requires a revisable object")
    state_limit = (args.native_object_state_limit, args.metatype_object_state_limit)
    if any(state_limit) and not all((*state_limit, metatype_object, args.metatype_revisable_object)):
        parser.error("The state limit fixture requires both Type Script binaries and a revisable Metatype object")
    if args.metatype_rule_registry and (not all(state_limit) or args.metatype_revision_topup):
        parser.error("The live rule fixture requires the state limit scripts and uses its own capacity profile")
    if args.metatype_decrease_ceiling and not args.metatype_rule_registry:
        parser.error("The decrease fixture requires a live ceiling registry")
    append_only = (args.native_object_append_only, args.metatype_object_append_only)
    if any(append_only) and not all((*append_only, metatype_object, args.metatype_revisable_object)):
        parser.error("The append-only fixture requires both Type Script binaries and a revisable Metatype object")
    workflow = (args.native_object_workflow, args.metatype_object_workflow)
    if any(workflow) and (not all((*workflow, metatype_object, args.metatype_revisable_object))
                         or any((*state_limit, *append_only, args.metatype_revision_topup))):
        parser.error("The workflow fixture requires both binaries and its own revisable Metatype profile")
    if args.metatype_workflow_early_return and not all(workflow):
        parser.error("An early workflow return requires both workflow binaries")
    royalties = (args.native_object_royalty, args.metatype_object_royalty)
    if any(royalties) and not all((*royalties, metatype_object)):
        parser.error("The royalty fixture requires both binaries and the Metatype object route")
    rotation = (args.native_object_rotating_approval, args.metatype_object_rotating_approval,
                args.metatype_approval_registry)
    if any(rotation) and not all((*rotation, metatype_object)):
        parser.error("The rotation fixture requires its registry, both adapters and the Metatype object route")
    if args.metatype_object_freeze and not all(rotation):
        parser.error("The freeze fixture requires both approval adapters and the registry")
    if args.metatype_approval_quorum and (not all(rotation)
            or not 1 <= args.metatype_approval_quorum[0] <= args.metatype_approval_quorum[1] <= 8):
        parser.error("The approval quorum requires a registry and 1 <= THRESHOLD <= KEYS <= 8")
    if args.metatype_shared_manifest and not (metatype_object or all(portable_udt)):
        parser.error("--metatype-shared-manifest requires the Metatype settlement fixture")
    if any((args.stem_native_object, args.object_policy, args.ckb_stem_object, *object_adapters)) and (
            not all((args.stem_native_object, args.object_policy, args.intercell_mailbox))
            or not (args.ckb_stem_object or metatype_object) or args.export_udt or args.stem_spore):
        parser.error("Native object settlement needs its Stem scripts and one CKB route")
    if args.wasm_fixture and (not args.intercell_mailbox or args.export_udt or args.stem_spore or args.native_spore_origin or args.stem_native_object):
        parser.error("--wasm-fixture uses the generic Intercell mailbox fixture")
    if args.exercise_publisher and (not args.intercell_mailbox or args.export_udt or args.stem_spore or args.native_spore_origin or args.stem_native_object):
        parser.error("--exercise-publisher uses the generic Intercell mailbox fixture")
    if args.full_udt_return and not args.export_udt:
        parser.error("--full-udt-return requires the UDT settlement fixture")
    if args.finish_udt_return and (not all(portable_udt) or args.full_udt_return):
        parser.error("--finish-udt-return exercises two successive Metatype redemptions")
    if (args.metatype_multiasset or args.metatype_composed_cell) and (
            not all(portable_udt) or args.full_udt_return or args.finish_udt_return
            or args.metatype_multiasset and args.metatype_composed_cell):
        parser.error("Choose one Metatype multiasset fixture and supply the Metatype UDT route")
    if (args.metatype_composed_partial or args.metatype_composed_selective) and not args.metatype_composed_cell:
        parser.error("partial and selective returns require --metatype-composed-cell")
    if args.intercell_mailbox and (args.reference_script or args.fee_funds or args.xudt or args.xudt_gate):
        parser.error("--intercell-mailbox uses the native CKB lane on its own")
    if args.security_domain and not args.reference_script:
        parser.error("--security-domain requires --reference-script")
    if args.reference_script and (args.fee_funds or args.xudt or args.xudt_gate):
        parser.error("--reference-script uses the native CKB lane without the fee-fund fixture")
    if bool(args.xudt) != bool(args.xudt_gate):
        parser.error("--xudt and --xudt-gate must be specified together")
    if args.checkpoint_fee_fund and not args.fee_funds:
        parser.error("--checkpoint-fee-fund requires --fee-funds")
    if args.fee_funds and args.xudt:
        parser.error("fee funds and xUDT use separate test rounds")
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    children = []
    try:
        report = bridge_round(args, l1, Fixtures(args), children)
        write_json(args.output / "jolteon-bridge-report.json", report)
        if args.stem_native_object:
            name = "Metatype custody" if args.metatype_dispatcher else "native object policy"
            print(f"Verified {name} and CKB round trip at agreed height {report['agreed']['height']}")
        elif args.native_spore_origin:
            print(f"Verified standard Spore export from Stem at agreed height {report['agreed']['height']}")
        elif args.stem_spore:
            print(f"Verified Spore settlement and original ID at agreed height {report['agreed']['height']}")
        elif args.export_udt:
            print(f"Verified Stem native UDT export and return at agreed height {report['agreed']['height']}")
        elif args.intercell_mailbox:
            print(f"Verified bidirectional Intercell messages at agreed height {report['agreed']['height']}")
        elif args.security_domain:
            print(f"Verified the real CKB security receipt in a Stem transaction at height {report['security_domain']['stem']['height']}")
        elif args.reference_script:
            print(f"Verified historical and live-at-anchor CKB references at agreed height {report['agreed']['height']}")
        elif args.fee_funds:
            print(f"Reconciled {len(report['fee_funds']['payouts'])} CKB fee payouts at agreed height {report['agreed']['height']}")
        else:
            print(f"Verified CKB payout {report['withdrawal']['l1_payout']} and agreed height {report['agreed']['height']}")
        if args.xudt:
            print(f"Verified xUDT payout {report['xudt']['withdrawal']['l1_payout']}")
    finally:
        for child in children:
            if child.poll() is None:
                child.terminate()
        for child in children:
            try:
                child.wait(timeout=8)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        l1.stop()


if __name__ == "__main__":
    main()

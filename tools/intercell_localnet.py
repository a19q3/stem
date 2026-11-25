"""Bidirectional Intercell fixture for the native Jolteon bridge runner."""

import json
import subprocess
import time

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output,
                             point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell, start, wait_for
from stake_localnet import witness

EMPTY = bytes(32)
MESSAGE_SEND = 0xff00
MESSAGE_RECEIVE = 0xff01


def intercell_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay, commands, children, *, publisher=None):
    client = clients[0]
    network, genesis = client.network, vault["genesis"]
    native_lock = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())),
                         fixture.key(fixture.owner)["blake160"], "data")
    native_owner = hex_bytes(script_hash(native_lock))
    secp = vault["secp"]
    code = args.intercell_mailbox.read_bytes()
    mailbox_capacity = 5000 * CKB
    code_capacity = (len(code) + 200) * CKB
    substitute_capacity = 512 * CKB
    funded = l1.send(fixture.sign("intercell-fund", transaction(
        [vault["change_point"]],
        [output(code_capacity, fixture.standard), output(mailbox_capacity, fixture.owner_lock),
         output(mailbox_capacity, fixture.owner_lock), output(substitute_capacity, fixture.standard),
         output(mailbox_capacity, fixture.owner_lock),
         output(vault["change"] - code_capacity - 3 * mailbox_capacity - substitute_capacity - FEE,
                fixture.standard)],
        [hex_bytes(code), "0x", "0x", "0x", "0x", "0x"], [secp]), [wallet_group(0, [fixture.faucet])]))
    mailbox_dep = dependency(point(funded, 0))
    fee_capacity = vault["change"] - code_capacity - 3 * mailbox_capacity - substitute_capacity - FEE
    type_id = ckb_hash(le(0, 8) + binary(funded) + le(1, 4) + le(0, 8))
    type_args = (type_id + binary(genesis) + binary(network) + script_hash(fixture.owner_lock)
                 + binary(vault["anchor"]["hash"]) + vault["anchor"]["id"])
    mailbox_type = script(hex_bytes(ckb_hash(code)), hex_bytes(type_args), "data2")
    mailbox_id = hex_bytes(script_hash(mailbox_type))
    foreign_type_id = ckb_hash(le(0, 8) + binary(funded) + le(2, 4) + le(0, 8))
    foreign_args = foreign_type_id + type_args[32:64] + bytes([77]) * 32 + type_args[96:]
    foreign_type = dict(mailbox_type, args=hex_bytes(foreign_args))
    alternate_id = ckb_hash(le(0, 8) + binary(funded) + le(4, 4) + le(0, 8))
    alternate_args = alternate_id + type_args[32:-32] + vault["foreign_anchor"]["id"]
    alternate_type = dict(mailbox_type, args=hex_bytes(alternate_args))

    def mailbox_state(nonce, root, outgoing=b""):
        return le(nonce, 8) + root + outgoing

    def ckb_tx(label, prev, capacity, data, *, evidence=None, checkpoint=None, headers=(), lock=None,
               type_=None, extra_input=False):
        inputs = [prev]
        outputs = [output(capacity, lock or fixture.owner_lock, type_ or mailbox_type)]
        outputs_data = [hex_bytes(data)]
        witnesses = [witness(evidence)] if evidence is not None else []
        groups = [wallet_group(0, [fixture.owner])]
        if extra_input:
            inputs.insert(0, point(funded, 5))
            outputs.insert(0, output(fee_capacity, fixture.standard))
            outputs_data.insert(0, "0x")
            witnesses.insert(0, "0x")
            groups = [wallet_group(0, [fixture.faucet]), wallet_group(1, [fixture.owner])]
        tx = transaction(inputs, outputs, outputs_data, [secp, mailbox_dep] + ([checkpoint] if checkpoint else []))
        tx["header_deps"] = list(headers)
        if witnesses:
            tx["witnesses"] = witnesses
        return fixture.sign(f"intercell-{label}", tx, groups)

    def ckb_reject(label, signed, code):
        result = l1.reject(signed)
        assert f"error code {code}" in result, (label, result)
        return result

    rejected = {}
    bad_initial = ckb_tx("bad-genesis", point(funded, 1), mailbox_capacity - FEE,
                         mailbox_state(0, EMPTY), headers=[l1.prove_committed(funded)["header"]["hash"]])
    rejected["false_ckb_genesis"] = ckb_reject("genesis", bad_initial, 81)
    ckb_start = l1.send(ckb_tx("create", point(funded, 1), mailbox_capacity - FEE,
                             mailbox_state(0, EMPTY), headers=[genesis]))
    foreign = l1.send(ckb_tx("create-foreign", point(funded, 2), mailbox_capacity - FEE,
                           mailbox_state(0, EMPTY), headers=[genesis], type_=foreign_type))
    alternate = l1.send(ckb_tx("create-alternate", point(funded, 4), mailbox_capacity - FEE,
                             mailbox_state(0, EMPTY), headers=[genesis], type_=alternate_type))
    ckb_head = point(ckb_start, 0)
    ckb_capacity = mailbox_capacity - FEE
    receive_count = 0
    root = EMPTY

    def prepare(label, source, destination, sender, receiver, nonce, payload):
        request = args.output / f"intercell-{label}-message.json"
        spec = dict(source=source, destination=destination, sender=sender, receiver=receiver,
                    nonce=nonce, payload=hex_bytes(payload))
        write_json(request, spec)
        return json.loads(run(args.binary, "intercell", "encode", "--request", request))

    outgoing = [prepare(label, network, genesis, native_owner, mailbox_id, nonce, payload)
                for label, nonce, payload in (("event", 41, b"reserve-observation"), ("reply", 8, b"status-request"))]
    bad_network = prepare("foreign", network, hex_bytes(bytes([77]) * 32), native_owner, mailbox_id, 41, b"same-nonce")
    bad_sender = prepare("wrong-sender", network, genesis, hex_bytes(bytes([32]) * 32), mailbox_id, 41, b"unauthorized")
    alternative_body = prepare("replacement-body", network, genesis, native_owner, mailbox_id, 41, b"different-payload")
    assert alternative_body["id"] == outgoing[0]["id"]

    def sign_stem(label, input_point, outputs, message=None, kind=MESSAGE_SEND, approvals=None):
        request, draft, context, lock = (args.output / f"intercell-{label}-{part}.json"
                                        for part in ("request", "draft", "context", "lock"))
        build = {"inputs": [{"out_point": input_point}], "outputs": outputs,
                 "cell_deps": [dependency(mint["lock_dep"])]}
        if message:
            build["messages"] = [{"group_index": 0, "actions": [{"kind": kind, "data": message["record"]}]}]
        write_json(request, build)
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request)))
        if approvals:
            attached = args.output / f"intercell-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references",
                "--genesis", spec_path, "--transaction", draft, "--observations", approvals)))
            draft = attached
        write_json(context, {"cells": [signing_cell(client, p) for p in (input_point, mint["lock_dep"])]})
        write_json(lock, native_lock)
        tx = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path, "--transaction", draft,
                            "--context", context, "--group-index", "0", "--lock", lock, "--key-file", fixture.owner))
        write_json(args.output / f"intercell-{label}-stem-signed.json", tx)
        return tx

    def stem_commit(tx):
        result = client.call("broadcast_tx_commit", tx=tx)
        assert result["check_tx"]["code"] == 0, result
        return result

    def stem_reject(tx, reason, peer=None):
        result = (peer or client).call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and reason in result["log"], result
        return result["log"]

    part = mint["payment_capacity"] // 6
    split = stem_commit(sign_stem("split", mint["payment"], [cell(part, native_lock)] * 5 + [cell(part - FEE, native_lock)]))
    inputs = [point("0x" + split["hash"], index) for index in range(6)]
    change = [cell(part - FEE, native_lock)]
    rejected["stem_cross_network"] = stem_reject(sign_stem("foreign", inputs[0], change, bad_network), "network mismatch")
    rejected["stem_sender_lock"] = stem_reject(sign_stem("wrong-sender", inputs[0], change, bad_sender), "sender or network mismatch")
    stems = [stem_commit(sign_stem(f"send-{i}", inputs[i], change, message)) for i, message in enumerate(outgoing)]
    rejected["stem_nonce_payload_substitution"] = stem_reject(
        sign_stem("send-replay", inputs[2], change, alternative_body), "nonce already used")

    def query(peer, path, data="0x"):
        result = peer.query(path, data=data, prove=True)
        return result["height"], json.loads(bytes.fromhex(result["value"]))

    def paired_proofs():
        values = [query(client, "/stem/intercell/outbox", msg["id"]) for msg in outgoing]
        return values if values[0][0] == values[1][0] else None

    proofs = wait_for(paired_proofs, "same-height Intercell outbox proofs")
    proof_height = proofs[0][0]
    for i, (_, proof) in enumerate(proofs):
        assert proof["record"] == outgoing[i]["record"]
    if publisher is None:
        checkpoint = relay.advance(proof_height, secp, probe=True)
    else:
        published = publisher.publish("anchor-intercell-outbox", proof_height)
        assert published["stem_height"] == proof_height, published
        checkpoint = dependency(point(published["transaction"], 0))
    header = args.output / "intercell-outbox-header.json"
    write_json(header, hex_bytes(relay.header(proof_height)))
    ckb_deliveries = []
    recoveries = {}

    def recover(label, head, *, mailbox=None, budget=None, reason=None):
        command = [args.binary, "intercell", "recover-inbox", "--l1-rpc", l1.url,
                   "--tx-hash", head["tx_hash"], "--output-index", str(int(head["index"], 16)),
                   "--mailbox", mailbox or mailbox_id]
        if budget is not None:
            command.extend(("--max-transactions", str(budget)))
        started = time.monotonic()
        result = subprocess.run(command, text=True, capture_output=True, timeout=20)
        elapsed = time.monotonic() - started
        if reason:
            assert result.returncode != 0 and reason in result.stderr, result
            rejected[label] = result.stderr
            return
        assert result.returncode == 0, result
        recovered = json.loads(result.stdout)
        assert recovered["head"] == head and recovered["mailbox"] == mailbox_id
        recoveries[label] = {**recovered, "seconds": elapsed, "command": [str(arg) for arg in command]}
        return recovered

    def delivery(i, label):
        _, proof = proofs[i]
        recovered = recover(label, ckb_head)
        assert recovered["expected_root"] == hex_bytes(root)
        assert recovered["consumed"] == [msg["id"] for msg in outgoing[:i]]
        request = args.output / f"intercell-delivery-{label}.json"
        write_json(request, {**proof, "consumed": recovered["consumed"], "expected_root": recovered["expected_root"]})
        result = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
        write_json(args.output / f"intercell-witness-{label}.json", result)
        return result

    recover("ckb_recovery_wrong_mailbox", ckb_head, mailbox=hex_bytes(script_hash(foreign_type)),
            reason="Type Script mismatch")
    recover("ckb_recovery_read_budget", ckb_head, budget=1, reason="transaction budget exhausted")

    replay_witness = None
    delivery_fork = None
    for i in (0, 1):
        result = delivery(i, f"before-delivery-{i}")
        raw = binary(result["input_type"])
        updated = binary(result["successor_root"])
        if i == 0:
            replay_witness = raw
            rejected["ckb_replay_to_foreign_mailbox"] = ckb_reject("foreign mailbox",
                ckb_tx("replay-foreign-mailbox", point(foreign, 0), mailbox_capacity - 2 * FEE,
                       mailbox_state(0, updated), evidence=raw, checkpoint=checkpoint, type_=foreign_type), 81)
            rejected["ckb_missing_checkpoint"] = ckb_reject("no checkpoint",
                ckb_tx("missing-checkpoint", ckb_head, ckb_capacity - FEE,
                       mailbox_state(0, updated), evidence=raw), 84)
            bad = bytearray(raw)
            bad[8 + len(binary(outgoing[i]["record"])) + 8] ^= 1
            rejected["ckb_forged_path"] = ckb_reject("path", ckb_tx("forged-path", ckb_head,
                ckb_capacity - FEE, mailbox_state(0, updated), evidence=bad, checkpoint=checkpoint), 84)
            cross = bytearray(raw)
            cross[8 + 32] ^= 1
            rejected["ckb_cross_network"] = ckb_reject("network", ckb_tx("foreign-destination", ckb_head,
                ckb_capacity - FEE, mailbox_state(0, updated), evidence=cross, checkpoint=checkpoint), 81)
            rejected["ckb_owner_substitution"] = ckb_reject("lock", ckb_tx("foreign-owner", ckb_head,
                ckb_capacity - FEE, mailbox_state(0, updated), evidence=raw, checkpoint=checkpoint,
                lock=fixture.friend_lock), 81)
        if i == 1:
            forked = ckb_tx("deliver-on-replaced-fork", ckb_head, ckb_capacity - FEE - 1,
                            mailbox_state(0, updated), evidence=raw, checkpoint=checkpoint, extra_input=True)
            orphan_head = point(l1.send(forked), 1)
            forked_inclusion = l1.prove_committed(orphan_head["tx_hash"])
            before = recover("before-delivery-reorg", orphan_head)
            assert before["expected_root"] == hex_bytes(updated)
            assert before["consumed"] == [msg["id"] for msg in outgoing]
            forked_header = forked_inclusion["header"]
            l1.call("truncate", forked_header["parent_hash"])
            l1.mine(spec["bridge"]["confirmations"] + 2)
            replacement = l1.call("get_block_hash", forked_header["number"])
            assert replacement != forked_header["hash"]
            recover("ckb_recovery_orphaned_delivery", orphan_head, reason="no longer live")
            delivery_fork = {"orphaned_head": orphan_head, "inclusion": forked_inclusion, "replacement": replacement}
            recovered_witness = delivery(i, "after-delivery-reorg")
            assert recovered_witness == result
        signed = ckb_tx(f"deliver-{i}", ckb_head, ckb_capacity - FEE,
                        mailbox_state(0, updated), evidence=raw, checkpoint=checkpoint, extra_input=(i == 1))
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        committed = l1.send(signed)
        ckb_deliveries.append({"transaction": committed, "cycles": cycles, "bytes": len(binary(signed["molecule"])),
                               "inclusion": l1.prove_committed(committed), "inbox_root": hex_bytes(updated)})
        ckb_head, ckb_capacity, root = point(committed, i), ckb_capacity - FEE, updated
    rejected["ckb_repeated_consumption"] = ckb_reject("replay", ckb_tx("repeat-delivery", ckb_head,
        ckb_capacity - FEE, mailbox_state(0, root), evidence=replay_witness, checkpoint=checkpoint), 85)

    def publish_ckb(label, target):
        nonlocal ckb_head, ckb_capacity, receive_count
        msg = prepare(label, genesis, network, mailbox_id, target, receive_count, label.encode())
        signed = ckb_tx(label, ckb_head, ckb_capacity - FEE,
                        mailbox_state(receive_count + 1, root, binary(msg["record"])))
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        ckb_head, ckb_capacity, receive_count = point(tx, 0), ckb_capacity - FEE, receive_count + 1
        return msg, {"point": ckb_head, "inclusion": l1.prove_committed(tx),
                     "cycles": cycles, "bytes": len(binary(signed["molecule"]))}

    def signers(subcommand, journal, arguments, reason=None):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            command = [args.binary, "bridge", subcommand, "--genesis", spec_path, "--l1-rpc", l1.url,
                       "--journal", args.output / f"intercell-{journal}-{index}", "--key-file", key, *arguments]
            if reason:
                result = subprocess.run(command, text=True, capture_output=True)
                assert result.returncode and reason in result.stderr, result
                return result.stderr
            votes.append(json.loads(run(*command)))
        return votes

    def anchors(previous, height, label):
        votes = signers("attest-anchor", "anchor", ["--previous-height", str(previous["height"]),
            "--previous-hash", previous["hash"], "--height", str(height)])
        path = args.output / f"intercell-{label}-anchor-approvals.json"
        write_json(path, votes)
        return json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", path))

    def reference(label, out_point, anchor, reason=None):
        request = args.output / f"intercell-{label}-reference.json"
        write_json(request, {"out_point": out_point, "anchor_height": anchor["height"],
                            "anchor_hash": anchor["hash"], "live_at_anchor": False})
        votes = signers("attest-reference", "observations", ["--request", request], reason)
        if reason:
            return votes
        path = args.output / f"intercell-{label}-approvals.json"
        write_json(path, votes)
        return path

    incoming, sent = publish_ckb("ckb-to-stem", native_owner)
    # A subsequent send spends that mailbox output before the guardians read it.
    wrong_receiver = hex_bytes(bytes([71]) * 32)
    wrong, misdirected = publish_ckb("ckb-to-other-receiver", wrong_receiver)
    counterfeit = l1.send(fixture.sign("intercell-untyped-l1-output", transaction(
        [point(funded, 3)], [output(substitute_capacity - FEE, fixture.owner_lock)],
        [hex_bytes(mailbox_state(1, EMPTY, binary(incoming["record"])))], [secp]),
        [wallet_group(0, [fixture.faucet])]))
    counterfeit_proof = l1.prove_committed(counterfeit)
    alternate_message = prepare("alternate-checkpoint", genesis, network, hex_bytes(script_hash(alternate_type)),
                                native_owner, 0, b"alternate checkpoint")
    alternate_send = l1.send(ckb_tx("alternate-checkpoint-send", point(alternate, 0), mailbox_capacity - 2 * FEE,
        mailbox_state(1, EMPTY, binary(alternate_message["record"])), type_=alternate_type))
    alternate_proof = l1.prove_committed(alternate_send)
    l1.mine(spec["bridge"]["confirmations"])
    _, old_anchor = query(client, "/stem/bridge-anchor")
    anchor_height = int(alternate_proof["header"]["number"], 16)
    advanced = stem_commit(anchors(old_anchor, anchor_height, "accepted"))
    _, anchor = query(client, "/stem/bridge-anchor")
    assert anchor["height"] == anchor_height
    approvals = reference("inbound", sent["point"], anchor)
    wrong_votes = reference("wrong-receiver", misdirected["point"], anchor)
    counterfeit_votes = reference("counterfeit", point(counterfeit, 0), anchor)
    alternate_votes = reference("alternate-checkpoint", point(alternate_send, 0), anchor)
    rejected["stem_alternate_checkpoint"] = stem_reject(sign_stem("alternate-checkpoint", inputs[2], change,
        alternate_message, MESSAGE_RECEIVE, alternate_votes), "differs from the certified mailbox")
    rejected["stem_non_mailbox_output"] = stem_reject(sign_stem("counterfeit", inputs[2], change, incoming,
        MESSAGE_RECEIVE, counterfeit_votes), "missing Intercell mailbox type")
    rejected["stem_receiver_lock"] = stem_reject(sign_stem("wrong-receiver", inputs[2], change, wrong,
        MESSAGE_RECEIVE, wrong_votes), "receiver or network mismatch")
    changed = prepare("inbound-forged", genesis, network, mailbox_id, native_owner, 0, b"changed")
    rejected["stem_forged_source_payload"] = stem_reject(sign_stem("inbound-forged", inputs[2], change, changed,
        MESSAGE_RECEIVE, approvals), "differs from the certified mailbox")
    forged_tx = bytearray(binary(sign_stem("inbound", inputs[2], change, incoming, MESSAGE_RECEIVE, approvals)))
    signature = binary(json.loads(approvals.read_text())[0]["signature"])
    forged_tx[forged_tx.index(signature)] ^= 1
    rejected["stem_forged_guardian"] = stem_reject(hex_bytes(forged_tx), "Signature")
    delivered_tx = sign_stem("inbound", inputs[2], [cell(part - FEE, native_lock, binary(incoming["record"]))],
                             incoming, MESSAGE_RECEIVE, approvals)
    delivered = stem_commit(delivered_tx)
    location = client.locations[delivered["hash"]]
    receipt_query = client.query("/stem/execution", data=hex_bytes(le(location["index"], 4)),
                                 height=location["height"], prove=True)
    receipt = json.loads(bytes.fromhex(receipt_query["value"]))
    assert binary(incoming["id"]) in binary(receipt["receipt"])
    received_cell = signing_cell(client, point("0x" + delivered["hash"], 0))
    assert binary(received_cell["cell"]).endswith(binary(incoming["record"]))
    rejected["stem_repeated_consumption"] = stem_reject(sign_stem("receive-replay", inputs[3], change, incoming,
                                      MESSAGE_RECEIVE, approvals), "nonce already used")

    restart_index = len(clients) - 1
    replica = clients[restart_index]
    wait_for(lambda: replica.status()["height"] >= delivered["height"], "recipient before restart")
    children[restart_index].terminate()
    children[restart_index].wait(timeout=8)
    started = time.monotonic()
    children[restart_index] = start(args, commands[restart_index], "intercell-restarted-replica.log")
    wait_for(lambda: replica.status()["height"] >= delivered["height"], "recipient after restart")
    recovery_seconds = time.monotonic() - started
    restart_proof = query(replica, "/stem/intercell/inbox", incoming["id"])[1]
    assert restart_proof["record"] == incoming["record"]
    rejected["stem_repeat_after_restart"] = stem_reject(sign_stem("restart-replay", inputs[4], change, incoming,
                                      MESSAGE_RECEIVE, approvals), "nonce already used", replica)

    restored = recover("after-stem-restart", ckb_head)
    assert restored["consumed"] == [msg["id"] for msg in outgoing]
    assert restored["expected_root"] == hex_bytes(root)
    surviving_head = ckb_head
    pending, fork_emission = publish_ckb("ckb-fork-message", native_owner)
    l1.mine(spec["bridge"]["confirmations"])
    fork_header = fork_emission["inclusion"]["header"]
    fork_anchor = {"height": int(fork_header["number"], 16), "hash": fork_header["hash"]}
    fork_certificate = reference("fork", fork_emission["point"], fork_anchor)
    orphan = sign_stem("fork", inputs[5], [cell(part - 2 * FEE, native_lock)], pending, MESSAGE_RECEIVE, fork_certificate)
    rejected["stem_unaccepted_anchor"] = stem_reject(orphan, "committed CKB anchor")
    l1.call("truncate", fork_header["parent_hash"])
    l1.mine(spec["bridge"]["confirmations"] + 2)
    replacement = l1.call("get_block_hash", fork_header["number"])
    assert replacement != fork_anchor["hash"]
    recover("ckb_recovery_orphaned_send", fork_emission["point"], reason="no longer live")
    assert recover("after-send-reorg", surviving_head) == restored
    rejected["guardian_orphaned_anchor"] = reference("post-reorg", fork_emission["point"], fork_anchor, "anchor was reorganized")
    rejected["stem_orphaned_message"] = stem_reject(orphan, "committed CKB anchor")
    assert query(client, "/stem/intercell/inbox", pending["id"])[1]["record"] is None
    assert query(client, "/stem/intercell/inbox", incoming["id"])[1]["record"] == incoming["record"]

    return {"mailbox": mailbox_type, "mailbox_id": mailbox_id, "creation": l1.prove_committed(ckb_start),
            "outgoing": outgoing, "stem_sends": stems, "outbox_proofs": proofs, "outbox_height": proof_height,
            "ckb_deliveries": ckb_deliveries, "l1_sends": [sent, misdirected], "incoming": incoming,
            "stem_delivery": delivered, "stem_delivery_bytes": len(binary(delivered_tx)),
            "stem_execution": receipt, "application_output": received_cell,
            "foreign_mailbox_creation": l1.prove_committed(foreign), "counterfeit": counterfeit_proof,
            "alternate_checkpoint_send": alternate_proof,
            "anchor_update": advanced, "anchor": anchor,
            "after_restart": restart_proof, "restart_seconds": recovery_seconds,
            "orphaned": fork_emission, "replacement": replacement, "delivery_fork": delivery_fork,
            "ckb_inbox_recovery": recoveries, "rejections": rejected}

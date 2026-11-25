"""Deliver a Stem message through a CKB checkpoint held by a successor committee."""

import json

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le,
                             output, point, script, script_hash, transaction, wallet_group)
from intercell_localnet import EMPTY, MESSAGE_SEND, encode_message, mailbox_state
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell
from stake_localnet import witness


class EpochMailbox:
    def __init__(self, args, l1, fixture, vault, spec, network):
        self.args, self.l1, self.fixture, self.vault, self.spec = args, l1, fixture, vault, spec
        self.network = network
        self.lock = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())),
                           fixture.key(fixture.owner)["blake160"], "data")
        self.sender = hex_bytes(script_hash(self.lock))
        code = args.intercell_mailbox.read_bytes()
        code_capacity = (len(code) + 200) * CKB
        balance = 1000 * CKB
        deploy = transaction([vault["change_point"]],
            [output(code_capacity, fixture.standard), output(balance, fixture.owner_lock),
             output(vault["change"] - code_capacity - balance - FEE, fixture.standard)],
            [hex_bytes(code), "0x", "0x"], [vault["secp"]])
        funding = l1.send(fixture.sign("epoch-mailbox-code", deploy, [wallet_group(0, [fixture.faucet])]))
        self.code = dependency(point(funding, 0))
        seed = point(funding, 1)
        type_id = ckb_hash(le(0, 8) + binary(funding) + le(1, 4) + le(0, 8))
        policy = (type_id + binary(vault["genesis"]) + binary(network) + script_hash(fixture.owner_lock)
                  + binary(vault["anchor"]["hash"]) + vault["anchor"]["id"])
        self.type = script(hex_bytes(ckb_hash(code)), hex_bytes(policy), "data2")
        self.id = hex_bytes(script_hash(self.type))
        self.balance = balance - FEE
        create = transaction([seed], [output(self.balance, fixture.owner_lock, self.type)],
                             [hex_bytes(mailbox_state(0, EMPTY))], [vault["secp"], self.code])
        create["header_deps"] = [vault["genesis"]]
        created = l1.send(fixture.sign("epoch-mailbox-create", create, [wallet_group(0, [fixture.owner])]))
        self.head = point(created, 0)
        self.creation = l1.prove_committed(created)

    def send(self, client, label, nonce, payload, previous, capacity, lock_dep):
        message = encode_message(self.args, label, self.network, self.vault["genesis"],
                                 self.sender, self.id, nonce, payload)
        request, context, draft, lock = (self.args.output / f"epoch-intercell-{label}-{suffix}.json"
                                        for suffix in ("request", "context", "draft", "lock"))
        write_json(request, {"inputs": [{"out_point": previous}],
             "outputs": [cell(capacity - FEE, self.lock)], "cell_deps": [dependency(lock_dep)],
             "messages": [{"group_index": 0, "actions": [{"kind": MESSAGE_SEND, "data": message["record"]}]}]})
        write_json(context, {"cells": [signing_cell(client, out_point) for out_point in (previous, lock_dep)]})
        write_json(draft, json.loads(run(self.args.binary, "wallet", "build", "--request", request)))
        write_json(lock, self.lock)
        signed = json.loads(run(self.args.binary, "wallet", "sign", "--genesis", self.spec,
                            "--transaction", draft, "--context", context, "--group-index", "0",
                            "--lock", lock, "--key-file", self.fixture.owner))
        write_json(self.args.output / f"epoch-intercell-{label}-signed.json", signed)
        result = client.call("broadcast_tx_commit", tx=signed)
        assert result["check_tx"]["code"] == 0, result
        return message, result

    @staticmethod
    def proof(client, message):
        query = client.query("/stem/intercell/outbox", data=message["id"], prove=True)
        proof = json.loads(bytes.fromhex(query["value"]))
        assert proof["record"] == message["record"], query
        return query["height"], proof

    def deliver(self, relay, message, earlier, current, checkpoint):
        old_height, old_proof = earlier
        new_height, new_proof = current
        assert old_height < self.args.blocks_per_epoch < new_height
        assert old_proof["proof"] != new_proof["proof"]
        root = self.args.output
        history_path = root / "epoch-intercell-history.json"
        history_command = [self.args.binary, "checkpoint", "history", "--archive-db", relay.archive,
                           "--index", relay.root / "history", "--network", self.network,
                           "--height", str(new_height), "--header", str(old_height)]
        history = json.loads(run(*history_command))
        write_json(history_path, history)

        def build(label, height, proof, historical=False):
            request, header_path = (root / f"epoch-intercell-{label}-{part}.json" for part in ("request", "header"))
            write_json(request, {**proof, "consumed": [], "expected_root": hex_bytes(EMPTY)})
            write_json(header_path, hex_bytes(relay.header(height)))
            command = [self.args.binary, "intercell", "delivery-witness", "--request", request, "--header", header_path]
            if historical:
                command += ["--history", history_path]
            bundle = json.loads(run(*command))
            write_json(root / f"epoch-intercell-{label}-witness.json", bundle)
            assert bundle["id"] == message["id"]
            return bundle

        stale = build("stale", old_height, old_proof)
        fresh = build("fresh", new_height, new_proof)
        historical = build("historical", new_height, old_proof, historical=True)
        assert stale["successor_root"] == fresh["successor_root"] == historical["successor_root"]

        def signed(label, bundle, head, balance):
            tx = transaction([head], [output(balance - FEE, self.fixture.owner_lock, self.type)],
                [hex_bytes(mailbox_state(0, binary(bundle["successor_root"])))],
                [self.vault["secp"], self.code, checkpoint])
            tx["witnesses"] = [witness(binary(bundle["input_type"]))]
            return self.fixture.sign(f"epoch-intercell-{label}", tx, [wallet_group(0, [self.fixture.owner])])

        rejected_stale = self.l1.reject(signed("reject-stale", stale, self.head, self.balance))
        assert "error code 84" in rejected_stale, rejected_stale
        control = signed("fresh-control", fresh, self.head, self.balance)
        actual = signed("historical-delivery", historical, self.head, self.balance)
        assert control["hash"] == actual["hash"]

        def cost(tx):
            return {"bytes": len(binary(tx["molecule"])),
                    "cycles": int(self.l1.call("estimate_cycles", tx["json"])["cycles"], 16)}

        fresh_cost, historical_cost = cost(control), cost(actual)
        delivered = self.l1.send(actual)
        successor = point(delivered, 0)
        rejected_repeat = self.l1.reject(signed("reject-repeat", historical, successor, self.balance - FEE))
        assert "error code 85" in rejected_repeat, rejected_repeat
        recover = [self.args.binary, "intercell", "recover-inbox", "--l1-rpc", self.l1.url,
                   "--tx-hash", delivered, "--output-index", "0", "--mailbox", self.id]
        recovered = json.loads(run(*recover))
        assert recovered["consumed"] == [message["id"]] and recovered["expected_root"] == historical["successor_root"]
        return {"mailbox": self.type, "creation": self.creation, "message": message,
                "original": {"height": old_height, "proof": old_proof},
                "later": {"height": new_height, "proof": new_proof, "checkpoint": checkpoint},
                "history": history, "history_command": list(map(str, history_command)),
                "fresh_control": fresh_cost, "historical_delivery": historical_cost,
                "inclusion": self.l1.prove_committed(delivered), "recovered": recovered,
                "rejections": {"stale_outbox": rejected_stale, "repeat": rejected_repeat}}

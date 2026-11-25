"""Shared Stake Registry transactions and observations for the native devnet."""

import json

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, binary, ckb_hash, dependency,
    hex_bytes, le, output, point, reject_cli, script, script_hash, transaction, wallet_group, write_key,
)
from localnet import run, write_json

EMPTY = bytes(32)
DOMAIN = b"Stem Stake Registry\0"
CHAIN = b"stem-stake-fixture"
DELAY = 20


def signable(operation, network, genesis, registry_id, old, new):
    return ckb_hash(operation + network + genesis + registry_id
                    + ckb_hash(DOMAIN + old) + ckb_hash(DOMAIN + new))


def guardian_group(n):
    return sorted(
        [Ed25519PrivateKey.from_private_bytes(bytes([n + i]) * 32) for i in range(4)],
        key=lambda key: key.public_key().public_bytes_raw(),
    )


def committee(guardians):
    return b"".join(key.public_key().public_bytes_raw() + le(1, 8) for key in guardians)


def generation(guardians):
    return ckb_hash(b"Stem CKB Stake authority\0" + committee(guardians))


def proof(guardians, message, limit=3):
    return le(len(guardians), 2) + committee(guardians) + le(limit, 2) + b"".join(
        le(index, 2) + key.sign(message) for index, key in enumerate(guardians[:limit])
    )


def witness(payload):
    size = 20 + len(payload)
    return hex_bytes(b"".join(le(value, 4) for value in (size, 16, 16, size, len(payload))) + payload)


def mk_record(key, amount, activation, exit_epoch, meta, withdrawn=False, frozen=False, *, delegated=0):
    commitment = EMPTY if withdrawn else ckb_hash(b"Stem CKB Stake Cell\0" + meta)
    fields = le(amount, 8) + le(activation, 8) + le(exit_epoch or 0, 8)
    return (key + fields[:16] + bytes([exit_epoch is not None]) + fields[16:] + bytes([frozen])
            + meta[32:48] + commitment + le(delegated, 8),
            fields + meta)


class Scenario:
    def __init__(self, args, l1, fixture):
        self.args = args
        self.l1 = l1
        self.fixture = fixture
        self.authority = guardian_group(1)
        self.sequence = 0
        self.records = {}
        self.retired = set()
        self.retired_root = EMPTY
        self.measurements = {}
        self.delegations = {}
        self.deploy()

    def deploy(self):
        block = self.l1.call("get_block_by_number", "0x0")
        self.genesis = binary(block["header"]["hash"])
        self.network = ckb_hash(b"Stem L1 stake local test" + self.genesis)
        faucet, groups = block["transactions"][:2]
        index, funds = next((idx, out) for idx, out in enumerate(faucet["outputs"])
                            if out["lock"]["args"] == FAUCET_ARG)
        self.secp = dependency(point(groups["hash"], 0), "dep_group")
        binary_code = self.args.stake_registry.read_bytes()
        capacity = (len(binary_code) + 200) * CKB
        funding = 12_000 * CKB
        checkpoint_code = self.args.direct_checkpoint.read_bytes()
        checkpoint_capacity = (len(checkpoint_code) + 200) * CKB
        relay_funding = 8_000 * CKB
        delegated = getattr(self.args, "delegation", False)
        self.relay_fee_capacity = (2000 if delegated else 200) * CKB
        self.delegate_fee_capacity = (1200 if delegated else 0) * CKB
        change = (int(funds["capacity"], 16) - capacity - funding - checkpoint_capacity
                  - 2 * relay_funding - self.relay_fee_capacity - self.delegate_fee_capacity - FEE)
        self.code_hash = hex_bytes(ckb_hash(binary_code))
        deployment = transaction(
            [point(faucet["hash"], index)],
            [output(value, self.fixture.standard) for value in
             (capacity, funding, change, checkpoint_capacity, relay_funding, relay_funding)]
            + [output(self.relay_fee_capacity, self.fixture.friend_lock)],
            [hex_bytes(binary_code), "0x", "0x", hex_bytes(checkpoint_code), "0x", "0x", "0x"],
            [self.secp],
        )
        if delegated:
            deployment["outputs"].append(output(self.delegate_fee_capacity, self.fixture.owner_lock))
            deployment["outputs_data"].append("0x")
        code_tx = self.l1.send(self.sign("deploy-stake", deployment, 0))
        self.code = dependency(point(code_tx, 0))
        checkpoint_hash = hex_bytes(ckb_hash(checkpoint_code))
        def anchor(index):
            return {
                "hash": checkpoint_hash,
                "id": ckb_hash(le(0, 8) + binary(code_tx) + le(index, 4) + le(0, 8)),
                "code": dependency(point(code_tx, 3)), "seed": point(code_tx, index),
                "genesis": hex_bytes(self.genesis), "network": hex_bytes(self.network),
            }

        self.checkpoint = anchor(4)
        self.foreign_checkpoint = anchor(5)
        self.relay_fee_point = point(code_tx, 6)
        if delegated:
            self.delegate_fee_point = point(code_tx, 7)
        type_id = ckb_hash(le(0, 8) + binary(code_tx) + le(1, 4) + le(0, 8))
        args = (type_id + self.network + self.genesis + le(100 * CKB, 8) + le(10, 8) + le(DELAY, 8)
                + binary(self.checkpoint["hash"]) + self.checkpoint["id"] + CHAIN)
        self.registry_type = script(self.code_hash, hex_bytes(args), "data2")
        self.registry_id = script_hash(self.registry_type)
        self.lock = script(self.code_hash, hex_bytes(self.registry_id + bytes(1)), "data2")
        self.registry_capacity = 1000 * CKB
        self.fee_capacity = funding - self.registry_capacity - FEE
        self.state = self.encode(0)
        tx = transaction(
            [point(code_tx, 1)],
            [output(self.registry_capacity, self.lock, self.registry_type),
             output(self.fee_capacity, self.fixture.standard)],
            [hex_bytes(self.state), "0x"],
            [self.secp, self.code],
        )
        self.registry_tx = self.l1.send(self.sign("create-registry", tx, 0))
        self.fee_point = point(self.registry_tx, 1)
        self.stake_points = {}

    def sign(self, label, tx, index, *, relayer=False):
        key = self.fixture.friend if relayer else self.fixture.faucet
        return self.fixture.sign(label, tx, [wallet_group(index, [key])])

    def encode(self, sequence, records=None, authority=None, root=None):
        if records is None:
            records = self.records
        if authority is None:
            authority = self.authority
        if root is None:
            root = self.retired_root
        return (self.network + le(sequence, 8) + generation(authority) + root + le(len(records), 2)
                + b"".join(records[key][0] for key in sorted(records)))

    def change(self, name, number, key, new_record=None, certificate=None, fee=FEE,
               since=0, expected=None, next_authority=None, capacity_adjustment=0, *, dependencies=(), measure=False, relayer=False):
        records = self.records.copy()
        root = self.retired_root
        path = b""
        if number in (0, 5):
            if key in self.retired and number == 0:
                path = bytes(32)
            else:
                archived = self.args.output / f"{name}-retired.json"
                write_json(archived, [hex_bytes(item) for item in sorted(self.retired)])
                marker = json.loads(run(self.args.binary, "stake", "retirement-proof", "--network", hex_bytes(self.network),
                                        "--validator", hex_bytes(key), "--retired", archived))
                if binary(marker["root"]) != root:
                    raise AssertionError("retired key history differs from the L1 Registry")
                path = binary(marker["proof"])
                if number == 5:
                    root = binary(marker["next"])
        if number == 5:
            records.pop(key)
        elif new_record is not None:
            if key in records:
                new_record = (new_record[0][:-8] + records[key][0][-8:], new_record[1])
            records[key] = new_record
        state = self.encode(self.sequence + 1, records, next_authority, root)
        payload = bytes([number])
        if key is not None:
            payload += key
        if certificate:
            payload += certificate(self.state, state)
        payload += path
        inputs = [point(self.registry_tx, 0)]
        outputs = [output(self.registry_capacity, self.lock, self.registry_type)]
        data = [hex_bytes(state)]
        change = (self.relay_fee_capacity if relayer else self.fee_capacity) - fee
        if key in self.stake_points:
            stake_point, capacity = self.stake_points[key]
            inputs.append(stake_point)
            change += capacity
        keep_stake = new_record is not None and number != 5
        if keep_stake:
            stake_type = script(self.code_hash, hex_bytes(self.registry_id + key), "data2")
            stake_data = new_record[1]
            bonded = int.from_bytes(stake_data[:8], "little")
            occupied = (8 + 33 + len(binary(self.lock["args"]))
                        + 33 + len(binary(stake_type["args"])) + len(stake_data)) * CKB
            capacity = bonded + occupied + capacity_adjustment
            outputs.append(output(capacity, self.lock, stake_type))
            data.append(hex_bytes(stake_data))
            change -= capacity
        fee_index = len(inputs)
        inputs.append(self.relay_fee_point if relayer else self.fee_point)
        fee_lock = self.fixture.friend_lock if relayer else self.fixture.standard
        outputs.append(output(change, fee_lock))
        data.append("0x")
        tx = transaction(inputs, outputs, data, [self.secp, self.code, *dependencies])
        if since:
            tx["inputs"][1]["since"] = hex(since)
        tx["witnesses"] = [witness(payload)]
        signed = self.sign(name, tx, fee_index, relayer=relayer)
        if expected:
            try:
                self.l1.call("send_transaction", signed["json"], "passthrough")
            except RuntimeError as error:
                if expected.lower() not in str(error).lower():
                    raise RuntimeError(f"{name} expected {expected}, got {error}") from error
                return str(error)
            raise RuntimeError(f"CKB accepted the invalid {name} transaction")
        if measure:
            self.measurements[name] = {
                "estimated_cycles": int(self.l1.call("estimate_cycles", signed["json"])["cycles"], 16),
                "payload_bytes": len(payload), "transaction_bytes": len(binary(signed["molecule"])),
            }
        self.registry_tx = self.l1.send(signed)
        self.sequence += 1
        self.records = records
        self.retired_root = root
        if number == 5:
            self.retired.add(key)
        self.state = state
        fee_point = point(self.registry_tx, len(outputs) - 1)
        if relayer:
            self.relay_fee_point, self.relay_fee_capacity = fee_point, change
        else:
            self.fee_point, self.fee_capacity = fee_point, change
        if keep_stake:
            self.stake_points[key] = (point(self.registry_tx, 1), capacity)
        elif key is not None:
            self.stake_points.pop(key)
        return self.registry_tx

    def delegation(self, name, operation, validator, amount=0, *, label="holder", expected=None,
                   separate_owner=False, funded_by_owner=True, wrong_recipient=False, registry_delta=None, since=None):
        prior = self.delegations.get(label)
        owner_lock = self.fixture.owner_lock if separate_owner else self.fixture.friend_lock
        if prior is None:
            registry_path = self.args.output / f"{name}-registry-type.json"
            input_path = self.args.output / f"{name}-first-input.json"
            write_json(registry_path, self.registry_type)
            write_json(input_path, {"since": "0x0", "previous_output": point(self.registry_tx, 0)})
            stake_type = json.loads(run(self.args.binary, "stake", "delegation-script",
                "--registry-type", registry_path, "--validator", hex_bytes(validator),
                "--owner", hex_bytes(script_hash(owner_lock)), "--first-input", input_path,
                "--output-index", "1"))
            previous_amount = 0
        else:
            stake_type, previous_amount, _, owner_lock = prior
        args = binary(stake_type["args"])
        occupied = (8 + 33 + len(binary(self.lock["args"])) + 33 + len(args) + 8) * CKB
        records = self.records.copy()
        record, own_data = records[validator]
        delta = amount - previous_amount if registry_delta is None else registry_delta
        records[validator] = (record[:-8] + le(int.from_bytes(record[-8:], "little") + delta, 8), own_data)
        next_state = self.encode(self.sequence + 1, records)
        inputs = [point(self.registry_tx, 0)]
        outputs = [output(self.registry_capacity, self.lock, self.registry_type)]
        data = [hex_bytes(next_state)]
        deps = [self.secp, self.code]
        headers = []
        payment = 0
        if prior:
            inputs.append(prior[2])
        if operation == 9:
            outputs.append(output(occupied + amount, self.lock, stake_type))
            data.append(hex_bytes(le(amount, 8)))
            fee = FEE
        else:
            payment = occupied + (previous_amount if operation == 10 else 0)
            outputs.append(output(payment, self.fixture.standard if wrong_recipient else owner_lock))
            data.append("0x")
            fee = FEE + (previous_amount if operation == 11 else 0)
            if operation == 10:
                stake, _ = self.stake_points[validator]
                header = self.l1.prove_committed(stake["tx_hash"])["header"]
                headers.append(header["hash"])
                deps.append(dependency(stake))
                if since is None:
                    since = int(header["number"], 16) + DELAY
        if not funded_by_owner:
            fee_point, available = self.fee_point, self.fee_capacity
            fee_key, fee_lock = self.fixture.faucet, self.fixture.standard
        elif owner_lock == self.fixture.owner_lock:
            fee_point, available = self.delegate_fee_point, self.delegate_fee_capacity
            fee_key, fee_lock = self.fixture.owner, self.fixture.owner_lock
        else:
            fee_point, available = self.relay_fee_point, self.relay_fee_capacity
            fee_key, fee_lock = self.fixture.friend, self.fixture.friend_lock
        change = available - fee
        change += (occupied + previous_amount if prior else 0) - (occupied + amount if operation == 9 else payment)
        inputs.append(fee_point)
        outputs.append(output(change, fee_lock))
        data.append("0x")
        tx = transaction(inputs, outputs, data, deps)
        tx["header_deps"] = headers
        if since is not None:
            tx["inputs"][1]["since"] = hex(since)
        tx["witnesses"] = [witness(bytes([operation]) + args)]
        signed = self.fixture.sign(name, tx, [wallet_group(len(inputs) - 1, [fee_key])])
        if expected:
            try:
                self.l1.call("send_transaction", signed["json"], "passthrough")
            except RuntimeError as error:
                if expected.lower() not in str(error).lower():
                    raise RuntimeError(f"{name} expected {expected}, got {error}") from error
                return str(error)
            raise RuntimeError(f"CKB accepted the invalid {name} transaction")
        self.measurements[name] = {
            "estimated_cycles": int(self.l1.call("estimate_cycles", signed["json"])["cycles"], 16),
            "transaction_bytes": len(binary(signed["molecule"])),
        }
        self.registry_tx = self.l1.send(signed)
        self.sequence += 1
        self.records = records
        self.state = next_state
        committed = self.l1.call("get_transaction", self.registry_tx)["transaction"]
        assert committed["outputs"][1] == outputs[1], committed
        fee_point = point(self.registry_tx, len(outputs) - 1)
        if not funded_by_owner:
            self.fee_point, self.fee_capacity = fee_point, change
        elif owner_lock == self.fixture.owner_lock:
            self.delegate_fee_point, self.delegate_fee_capacity = fee_point, change
        else:
            self.relay_fee_point, self.relay_fee_capacity = fee_point, change
        if operation == 9:
            self.delegations[label] = (stake_type, amount, point(self.registry_tx, 1), owner_lock)
        else:
            del self.delegations[label]
        return {"transaction": self.registry_tx, "inclusion": self.l1.prove_committed(self.registry_tx),
                "principal": amount if operation == 9 else previous_amount,
                "occupied": occupied, "recipient_paid": payment, "fee": fee, "type": stake_type}

    def observe(self, label, target_epoch, parent=None, missing=False, count=1, signing_anchor=None,
                rotation=None):
        self.l1.mine(2)
        path = self.args.output / f"{label}-observe.json"
        request = {
            "stem_network_id": hex_bytes(self.network), "ckb_genesis": hex_bytes(self.genesis),
            "registry_type": self.registry_type, "registry": point(self.registry_tx, 0),
            "stake_cells": [self.stake_points[key][0] for key in sorted(self.stake_points)] if not missing else [],
            "confirmations": 2, "anchor_height": int(self.l1.call("get_tip_block_number"), 16) - 2,
            "target_epoch": target_epoch, "parent": parent,
            "guardians": [
                {"ed25519": hex_bytes(key.public_key().public_bytes_raw()), "power": 1}
                for key in self.authority
            ],
        }
        signers = [(key, False) for key in self.authority[:count]]
        if rotation:
            previous, departing = rotation
            request["rotation"] = {
                "previous_registry": previous,
                "departing_guardians": [
                    {"ed25519": hex_bytes(key.public_key().public_bytes_raw()), "power": 1}
                    for key in departing
                ],
            }
            signers += [(key, True) for key in departing[:count]]
        write_json(path, request)
        def anchor(value):
            return (le(value["height"], 8) + binary(value["block_hash"])
                    + le(value["sequence"], 8) + binary(value["commitment"]))

        votes = []
        for index, (key, leaving) in enumerate(signers):
            name = f"stake-observer-{index}"
            if rotation or (self.args.output / f"{name}.key").exists() and not leaving:
                existing = self.args.output / f"{name}.key"
                if rotation or existing.read_bytes() != key.private_bytes_raw():
                    name = f"stake-observer-{key.public_key().public_bytes_raw().hex()}"
            key_path = self.args.output / f"{name}.key"
            if not key_path.exists():
                key_path = write_key(self.args.output, name, key.private_bytes_raw())
            command = [
                self.args.binary, "stake", "observe", "--request", path, "--l1-rpc", self.l1.url,
                "--journal", self.args.output / f"stake-journal-{index}", "--key-file", key_path,
            ]
            if signing_anchor:
                command.extend(["--stem-signing-anchor", signing_anchor])
            if leaving:
                command.append("--departing")
            if missing:
                return reject_cli("incomplete Stake Cell set", *command)
            result = json.loads(run(*command))
            if binary(result["registry_data"]) != self.state:
                raise RuntimeError("Guardian signed a different Registry")
            message = ckb_hash(
                b"Stem guardian Stake Snapshot\0" + self.network + self.genesis + self.registry_id
                + generation(self.authority) + le(target_epoch, 8) + anchor(result["registry"])
                + bytes([parent is not None]) + (anchor(parent) if parent else b"")
            )
            key.public_key().verify(binary(result["attestation"]["signature"]), message)
            votes.append(result)
        return votes

    def verify_genesis_snapshot(self, votes):
        trust = self.args.output / "snapshot-trust.json"
        write_json(trust, {
            "stem_network_id": hex_bytes(self.network), "ckb_genesis": hex_bytes(self.genesis),
            "registry_id": hex_bytes(self.registry_id), "target_epoch": 0, "previous": None,
            "minimum_bond": 100 * CKB, "shannons_per_power": CKB,
            "max_validators": 4, "max_total_power": 800 if getattr(self.args, "delegation_power_cap", False) else 1000,
            "blocks_per_epoch": 10,
            "guardians": [
                {"ed25519": hex_bytes(key.public_key().public_bytes_raw()), "power": 1}
                for key in self.authority
            ],
        })
        path = self.args.output / "genesis-observations.json"
        command = [self.args.binary, "stake", "verify", "--trust", trust, "--observations", path]
        write_json(path, votes[:2])
        rejected = reject_cli("Quorum", *command)
        write_json(path, votes)
        return json.loads(run(*command)), rejected

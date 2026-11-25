#!/usr/bin/env python3
"""Deploy and exercise the Stake Registry on an isolated CKB dev chain."""

import argparse
import hashlib
import json
from pathlib import Path
import struct

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, Fixtures, L1, binary, ckb_hash, dependency,
    hex_bytes, le, output, point, reject_cli, script, script_hash, transaction, wallet_group, write_key,
)
from localnet import run, write_json

EMPTY = bytes(32)
DOMAIN = b"Fern Stake Registry\0"
CHAIN = b"fern-stake-fixture"
BOND = 200 * CKB
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
    return ckb_hash(b"Fern CKB Stake authority\0" + committee(guardians))


def proof(guardians, message, limit=3):
    return le(len(guardians), 2) + committee(guardians) + le(limit, 2) + b"".join(
        le(index, 2) + key.sign(message) for index, key in enumerate(guardians[:limit])
    )


def witness(payload):
    size = 20 + len(payload)
    return hex_bytes(b"".join(le(value, 4) for value in (size, 16, 16, size, len(payload))) + payload)


def vote(i, validator):
    block = bytes([i + 1]) * 32
    parts = bytes([i + 5]) * 32
    # CometBFT 0.38.26 VoteSignBytes output for height 42, round 0 and two block IDs.
    canonical = (
        bytes.fromhex("730802112a0000000000000022480a20") + block
        + bytes.fromhex("122408021220") + parts
        + bytes.fromhex("2a080880e2cfaa0610") + bytes([13 + i])
        + bytes.fromhex("3212") + CHAIN
    )
    return struct.pack("<BqiqiBI", 2, 42, 0, 1_700_000_000, 13 + i, 1, 2) + block + parts + validator.sign(canonical)


def mk_record(key, amount, activation, exit_epoch, meta, withdrawn=False, frozen=False):
    commitment = EMPTY if withdrawn else ckb_hash(b"Fern CKB Stake Cell\0" + meta)
    fields = le(amount, 8) + le(activation, 8) + le(exit_epoch or 0, 8)
    return (key + fields[:16] + bytes([exit_epoch is not None]) + fields[16:] + bytes([frozen]) + meta[32:48] + commitment,
            fields + meta)


class Scenario:
    def __init__(self, args, l1, fixture):
        self.args = args
        self.l1 = l1
        self.fixture = fixture
        self.authority = guardian_group(1)
        self.sequence = 0
        self.records = {}
        self.deploy()

    def deploy(self):
        block = self.l1.call("get_block_by_number", "0x0")
        self.genesis = binary(block["header"]["hash"])
        self.network = ckb_hash(b"Fern L1 stake local test" + self.genesis)
        faucet, groups = block["transactions"][:2]
        index, funds = next((idx, out) for idx, out in enumerate(faucet["outputs"])
                            if out["lock"]["args"] == FAUCET_ARG)
        self.secp = dependency(point(groups["hash"], 0), "dep_group")
        binary_code = self.args.stake_registry.read_bytes()
        capacity = (len(binary_code) + 200) * CKB
        funding = 12_000 * CKB
        change = int(funds["capacity"], 16) - capacity - funding - FEE
        self.code_hash = hex_bytes(ckb_hash(binary_code))
        deployment = transaction(
            [point(faucet["hash"], index)],
            [output(value, self.fixture.standard) for value in (capacity, funding, change)],
            [hex_bytes(binary_code), "0x", "0x"],
            [self.secp],
        )
        code_tx = self.l1.send(self.sign("deploy-stake", deployment, 0))
        self.code = dependency(point(code_tx, 0))
        type_id = ckb_hash(le(0, 8) + binary(code_tx) + le(1, 4) + le(0, 8))
        args = type_id + self.network + self.genesis + le(100 * CKB, 8) + le(10, 8) + le(DELAY, 8) + CHAIN
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

    def sign(self, label, tx, index):
        return self.fixture.sign(label, tx, [wallet_group(index, [self.fixture.faucet])])

    def encode(self, sequence, records=None, authority=None):
        if records is None:
            records = self.records
        if authority is None:
            authority = self.authority
        return (self.network + le(sequence, 8) + generation(authority) + le(len(records), 2)
                + b"".join(records[key][0] for key in sorted(records)))

    def change(self, name, number, key, new_record=None, certificate=None, fee=FEE,
               since=0, expected=None, next_authority=None, capacity_adjustment=0):
        records = self.records.copy()
        if new_record is not None:
            records[key] = new_record
        state = self.encode(self.sequence + 1, records, next_authority)
        payload = bytes([number])
        if key is not None:
            payload += key
        if certificate:
            payload += certificate(self.state, state)
        inputs = [point(self.registry_tx, 0)]
        outputs = [output(self.registry_capacity, self.lock, self.registry_type)]
        data = [hex_bytes(state)]
        change = self.fee_capacity - fee
        if key in self.stake_points:
            stake_point, capacity = self.stake_points[key]
            inputs.append(stake_point)
            change += capacity
        keep_stake = new_record is not None and new_record[0][-32:] != EMPTY
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
        owner_index = len(inputs)
        inputs.append(self.fee_point)
        outputs.append(output(change, self.fixture.standard))
        data.append("0x")
        tx = transaction(inputs, outputs, data, [self.secp, self.code])
        if since:
            tx["inputs"][1]["since"] = hex(since)
        tx["witnesses"] = [witness(payload)]
        signed = self.sign(name, tx, owner_index)
        if expected:
            try:
                self.l1.call("send_transaction", signed["json"], "passthrough")
            except RuntimeError as error:
                if expected.lower() not in str(error).lower():
                    raise RuntimeError(f"{name} expected {expected}, got {error}") from error
                return str(error)
            raise RuntimeError(f"CKB accepted the invalid {name} transaction")
        self.registry_tx = self.l1.send(signed)
        self.sequence += 1
        self.records = records
        self.state = state
        self.fee_point = point(self.registry_tx, len(outputs) - 1)
        self.fee_capacity = change
        if keep_stake:
            self.stake_points[key] = (point(self.registry_tx, 1), capacity)
        elif key is not None:
            self.stake_points.pop(key)
        return self.registry_tx

    def observe(self, label, target_epoch, parent=None, missing=False, count=1, signing_anchor=None):
        self.l1.mine(2)
        path = self.args.output / f"{label}-observe.json"
        request = {
            "fern_network_id": hex_bytes(self.network), "ckb_genesis": hex_bytes(self.genesis),
            "registry_type": self.registry_type, "registry": point(self.registry_tx, 0),
            "stake_cells": [self.stake_points[key][0] for key in sorted(self.stake_points)] if not missing else [],
            "confirmations": 2, "target_epoch": target_epoch, "parent": parent,
            "guardians": [
                {"ed25519": hex_bytes(key.public_key().public_bytes_raw()), "power": 1}
                for key in self.authority
            ],
        }
        write_json(path, request)
        def anchor(value):
            return (le(value["height"], 8) + binary(value["block_hash"])
                    + le(value["sequence"], 8) + binary(value["commitment"]))

        votes = []
        for index, key in enumerate(self.authority[:count]):
            key_path = self.args.output / f"stake-observer-{index}.key"
            if not key_path.exists():
                key_path = write_key(self.args.output, f"stake-observer-{index}", key.private_bytes_raw())
            command = [
                self.args.binary, "stake", "observe", "--request", path, "--l1-rpc", self.l1.url,
                "--journal", self.args.output / f"stake-journal-{index}", "--key-file", key_path,
            ]
            if signing_anchor:
                command.extend(["--fern-signing-anchor", signing_anchor])
            if missing:
                return reject_cli("incomplete Stake Cell set", *command)
            result = json.loads(run(*command))
            if binary(result["registry_data"]) != self.state:
                raise RuntimeError("Guardian signed a different Registry")
            message = ckb_hash(
                b"Fern guardian Stake Snapshot\0" + self.network + self.genesis + self.registry_id
                + generation(self.authority) + le(target_epoch, 8) + anchor(result["registry"])
                + bytes([parent is not None]) + (anchor(parent) if parent else b"")
            )
            key.public_key().verify(binary(result["attestation"]["signature"]), message)
            votes.append(result)
        return votes

    def verify_genesis_snapshot(self, votes):
        trust = self.args.output / "snapshot-trust.json"
        write_json(trust, {
            "fern_network_id": hex_bytes(self.network), "ckb_genesis": hex_bytes(self.genesis),
            "registry_id": hex_bytes(self.registry_id), "target_epoch": 0, "previous": None,
            "minimum_bond": 100 * CKB, "shannons_per_power": CKB,
            "max_validators": 4, "max_total_power": 1000, "blocks_per_epoch": 10,
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

    def report(self):
        validator = Ed25519PrivateKey.from_private_bytes(
            hashlib.sha256(b"fern staking evidence fixture").digest())
        key = validator.public_key().public_bytes_raw()
        assert key.hex() == "8b143b3cd645c965157b8ace56bcf2605a087d1195db38010b745976ce63724a"
        owner = script_hash(self.fixture.standard)
        zero = owner + bytes(48)
        registered = mk_record(key, BOND, 0, None, zero)

        def message(domain, old, new):
            return signable(domain, self.network, self.genesis, self.registry_id, old, new)

        def register_signature(old, new):
            return validator.sign(message(b"Fern CKB Stake registration\0", old, new))

        results = {"l1_genesis": hex_bytes(self.genesis), "registry_type": self.registry_type,
                   "registry_hash": hex_bytes(self.registry_id), "registry_creation": self.registry_tx}
        results["underfunded_rejected"] = self.change(
            "underfunded", 0, key, registered, register_signature,
            expected="ValidationFailure", capacity_adjustment=-1)
        results["registered"] = self.change("register", 0, key, registered, register_signature)
        results["duplicate_rejected"] = self.change(
            "duplicate-register", 0, key, mk_record(key, BOND, 2, None, zero),
            register_signature, expected="ValidationFailure")
        amount = BOND + 50 * CKB
        results["increased"] = self.change("increase", 1, key, mk_record(key, amount, 0, None, zero))

        def certify(domain, limit=3):
            return lambda old, new: proof(self.authority, message(domain, old, new), limit)

        active = owner + le(1, 8) + le(0, 8) + EMPTY
        results["activated"] = self.change(
            "activate", 3, key, mk_record(key, amount, 0, None, active),
            certify(b"Fern CKB Stake activation\0"))
        results["incomplete_snapshot_rejected"] = self.observe("omitted", 0, missing=True)
        votes = self.observe("activated", 0, count=3)
        results["snapshot_at_activation"] = votes[0]
        verified, quorum_reject = self.verify_genesis_snapshot(votes)
        if verified["validators"] != [{"ed25519": hex_bytes(key), "power": amount // CKB}]:
            raise RuntimeError("Incorrect stake-derived validator power")
        results["verified_genesis_snapshot"] = verified
        results["snapshot_minority_rejected"] = quorum_reject
        results["exit_requested"] = self.change(
            "request-exit", 2, key, mk_record(key, amount, 0, 6, active))
        results["exit_without_retirement_rejected"] = self.change(
            "withdraw-without-retirement", 5, key, mk_record(key, 0, 0, 6, active, True),
            since=(1 << 63), expected="ValidationFailure")
        penalty = ckb_hash(b"Fern CometBFT double vote\0" + self.network
                           + le(len(CHAIN), 4) + CHAIN + key + le(42, 8) + le(0, 4) + bytes([2]))
        frozen = owner + le(1, 8) + le(0, 8) + penalty
        votes = vote(0, validator) + vote(1, validator)
        frozen_record = mk_record(key, amount, 0, 6, frozen, frozen=True)
        results["forged_vote_rejected"] = self.change(
            "forged-votes", 6, key, frozen_record,
            lambda *_: votes[:-1] + bytes([votes[-1] ^ 1]), expected="ValidationFailure")
        results["frozen"] = self.change("freeze", 6, key, frozen_record, lambda *_: votes)
        results["frozen_shannons"] = amount
        results["snapshot_after_freeze"] = self.observe(
            "frozen", 1, results["snapshot_at_activation"]["registry"])[0]
        results["replay_rejected"] = self.change(
            "replay-slash", 6, key, frozen_record, lambda *_: votes, expected="ValidationFailure")
        results["slash_before_retirement_rejected"] = self.change(
            "slash-before-retirement", 8, key, mk_record(key, 0, 0, 6, frozen, frozen=True),
            fee=amount + FEE, expected="ValidationFailure")
        retired = owner + le(1, 8) + le(61, 8) + penalty
        retired_record = mk_record(key, amount, 0, 6, retired, frozen=True)
        results["minority_retirement_rejected"] = self.change(
            "minority-retirement", 4, key, retired_record,
            certify(b"Fern CKB Stake retirement\0", 2), expected="ValidationFailure")
        results["retired"] = self.change(
            "retire", 4, key, retired_record, certify(b"Fern CKB Stake retirement\0"))
        results["pending_penalty_withdrawal_rejected"] = self.change(
            "withdraw-before-slash", 5, key, mk_record(key, 0, 0, 6, retired, True, frozen=True),
            expected="ValidationFailure")
        slash_record = mk_record(key, 0, 0, 6, retired, frozen=True)
        results["unfunded_penalty_rejected"] = self.change(
            "unfunded-penalty", 8, key, slash_record, expected="ValidationFailure")
        results["slashed"] = self.change("slash", 8, key, slash_record, fee=amount + FEE)
        results["slashed_shannons"] = amount
        results["penalty_id"] = hex_bytes(penalty)
        results["consensus_time_lock_rejected"] = self.change(
            "ckb-delay-not-elapsed", 5, key, mk_record(key, 0, 0, 6, retired, True, frozen=True),
            since=(1 << 63) | DELAY, expected="Immature")
        self.l1.mine(DELAY)
        results["withdrawn"] = self.change(
            "withdraw", 5, key, mk_record(key, 0, 0, 6, retired, True, frozen=True),
            since=(1 << 63) | DELAY)
        results["reregistration_rejected"] = self.change(
            "reregister", 0, key, registered, register_signature, expected="ValidationFailure")
        next_authority = guardian_group(11)

        def rotation(old, new):
            digest = message(b"Fern CKB Stake authority rotation\0", old, new)
            return proof(self.authority, digest) + proof(next_authority, digest)

        results["authority_rotated"] = self.change(
            "rotate-authority", 7, None, certificate=rotation, next_authority=next_authority)
        old_authority, self.authority = self.authority, next_authority
        second = Ed25519PrivateKey.from_private_bytes(bytes([90]) * 32)
        key = second.public_key().public_bytes_raw()
        results["second_registration"] = self.change(
            "register-second", 0, key, mk_record(key, BOND, 7, None, zero),
            lambda old, new: second.sign(message(b"Fern CKB Stake registration\0", old, new)))
        activated = owner + le(71, 8) + le(0, 8) + EMPTY
        results["departed_guardians_rejected"] = self.change(
            "departed-guardians", 3, key, mk_record(key, BOND, 7, None, activated),
            lambda old, new: proof(old_authority, message(b"Fern CKB Stake activation\0", old, new)),
            expected="ValidationFailure")
        self.change("activate-second", 3, key, mk_record(key, BOND, 7, None, activated),
                    certify(b"Fern CKB Stake activation\0"))
        self.change("exit-second", 2, key, mk_record(key, BOND, 7, 8, activated))
        retired = owner + le(71, 8) + le(81, 8) + EMPTY
        self.change("retire-second", 4, key, mk_record(key, BOND, 7, 8, retired),
                    certify(b"Fern CKB Stake retirement\0"))
        self.l1.mine(DELAY)
        before = self.fee_capacity
        _, stake_capacity = self.stake_points[key]
        results["unslashed_withdrawal"] = self.change(
            "withdraw-second", 5, key, mk_record(key, 0, 7, 8, retired, True),
            since=(1 << 63) | DELAY)
        if self.fee_capacity != before + stake_capacity - FEE:
            raise RuntimeError("Withdrawal did not return the full Stake Cell to the owner")
        results["unslashed_withdrawal_bonded_shannons"] = BOND
        results["final_sequence"] = self.sequence
        results["slash_inclusion"] = self.l1.prove_committed(results["slashed"])
        results["withdrawal_inclusion"] = self.l1.prove_committed(results["withdrawn"])
        observed = results["snapshot_after_freeze"]["registry"]
        height = observed["height"]
        self.l1.call("truncate", self.l1.call("get_block_hash", hex(height - 1)))
        self.l1.mine(3)
        replacement = self.l1.call("get_block_hash", hex(height))
        if replacement == observed["block_hash"]:
            raise AssertionError("CKB test node reproduced the original anchor")
        results["reorganized_anchor_rejected"] = reject_cli(
            "previous L1 anchor was reorganized",
            self.args.binary, "stake", "observe",
            "--request", self.args.output / "frozen-observe.json",
            "--l1-rpc", self.l1.url,
            "--journal", self.args.output / "stake-journal-0",
            "--key-file", self.args.output / "stake-observer-0.key",
        )
        results["replacement_l1_hash"] = replacement
        return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "l1", "stake-registry", "output"):
        parser.add_argument("--" + flag, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, default=42000)
    args = parser.parse_args()
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    try:
        results = Scenario(args, l1, Fixtures(args)).report()
        write_json(args.output / "stake-round-trip.json", results)
        print(json.dumps(results, indent=2))
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

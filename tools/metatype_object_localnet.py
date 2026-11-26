#!/usr/bin/env python3
"""Exercise independently sealed Metatype objects on a real CKB development chain."""

import argparse
from dataclasses import dataclass
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes,
    le, output, packed_script, point, script, script_hash, transaction, wallet_group,
)
from localnet import write_json
from metatype_localnet import capacity, confirmed, metatype_data
from metatype_polytype_fixture import exercise_polytype
from spore_settlement_localnet import molecule_bytes, table

WITNESS_MAGIC = b"Stem Metatype Witness\0"
APPROVAL_DOMAIN = b"Stem Metatype approval\0"


@dataclass
class Object:
    identity: bytes
    approval: bytes
    key: Ed25519PrivateKey
    state: bytes

    def manifest(self, omit_approval=False):
        entries = [(self.identity, self.state)]
        if not omit_approval:
            entries.append((self.approval, b""))
        return metatype_data(entries)


def object_for(codes, deployment, seed, index, key, state):
    public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    packed_seed = binary(seed["tx_hash"]) + le(int(seed["index"], 16), 4)
    identity = ckb_hash(b"Stem object identity\0" + deployment + packed_seed + le(index, 4))
    obj = packed_script(script(hex_bytes(codes[0]), hex_bytes(identity + public + codes[1]), "data"))
    approval = packed_script(script(hex_bytes(codes[1]), hex_bytes(ckb_hash(obj) + public), "data"))
    return Object(obj, approval, key, state)


def approval(obj, deployment, tx_hash, action, recipient):
    group = ckb_hash(obj.approval)
    proof = ckb_hash(obj.state)
    digest = ckb_hash(APPROVAL_DOMAIN + deployment + group + tx_hash + le(action, 2)
                      + le(len(recipient), 4) + recipient + le(len(proof), 4) + proof)
    seal = obj.key.sign(digest)
    return (deployment, group, action, recipient, proof, seal)


def encode(entries, canonical=True):
    ordered = sorted(entries, key=lambda row: row[:2]) if canonical else entries
    return WITNESS_MAGIC + le(len(ordered), 1) + b"".join(
        deployment + group + le(action, 2)
        + b"".join(le(len(field), 2) + field for field in (message, proof, seal))
        for deployment, group, action, message, proof, seal in ordered
    )


def exercise(args, l1, fixture):
    genesis = l1.call("get_block_by_number", "0x0")
    genesis_hash = genesis["header"]["hash"]
    faucet, system = genesis["transactions"][:2]
    faucet_index, faucet_cell = next(
        (i, cell) for i, cell in enumerate(faucet["outputs"]) if cell["lock"]["args"] == FAUCET_ARG
    )
    default_deps = [dependency(point(system["hash"], 0), "dep_group")]
    programs = [args.dispatcher.read_bytes(), args.object.read_bytes(), args.approval.read_bytes()]
    code, *adapters = [ckb_hash(program) for program in programs]
    adapter_table = bytes([2]) + b"".join(sorted(h + b"\0" + h for h in adapters))
    programs.append(adapter_table)
    deposits = [(len(data) + 100) * CKB for data in programs]
    funding = int(faucet_cell["capacity"], 16) - sum(deposits) - FEE
    tx = transaction([point(faucet["hash"], faucet_index)],
                     [output(n, fixture.standard) for n in [*deposits, funding]],
                     [hex_bytes(data) for data in programs] + ["0x"], default_deps)
    deployment = confirmed(l1, fixture.sign("deploy", tx, [wallet_group(0, [fixture.faucet])]))
    deployed = deployment["transaction"]
    fee = point(deployed, len(deposits))
    deps = default_deps + [dependency(point(deployed, i)) for i in range(len(deposits))]
    physical_args = (binary(genesis_hash) + ckb_hash(b"Metatype object fixture\0") + ckb_hash(adapter_table))
    physical = script(hex_bytes(code), hex_bytes(physical_args), "data2")
    deployment_hash = script_hash(physical)
    keys = [Ed25519PrivateKey.from_private_bytes(ckb_hash(b"Metatype fixture authority" + bytes([i])))
            for i in range(2)]
    make_state = lambda content: table(molecule_bytes(b"text/plain"), molecule_bytes(content), b"")
    contents = [make_state(b"Stem object with two Type Groups"), make_state(b"metatype object B")]
    objects = [object_for(adapters, deployment_hash, fee, i, keys[i], body)
               for i, body in enumerate(contents)]
    policy_entries = {}
    results = {}
    rejections = {}

    def build(label, inputs, objects, *, action, recipient, lock_key, budget, edit=None,
              data=None, headers=None, ty=physical):
        rows = [obj.manifest() for obj in objects] if data is None else data
        deposits = [capacity(recipient, ty, row) for row in rows]
        change = budget - sum(deposits) - FEE
        if action == 1:
            change_lock = fixture.owner_lock
        else:
            change_lock = recipient
        tx = transaction(inputs, [output(n, recipient, ty) for n in deposits] + [output(change, change_lock)],
                         [hex_bytes(row) for row in rows] + ["0x"], deps)
        tx["header_deps"] = [genesis_hash] if headers is None else headers
        draft = fixture.sign(label + "-raw", tx, [])
        signatures = [approval(obj, script_hash(ty), binary(draft["hash"]),
                               action, script_hash(recipient)) for obj in objects]
        policy_entries[label] = signatures
        wire = encode(signatures) if edit is None else edit(signatures)
        tx["witnesses"] = ["0x"] * len(inputs) + [hex_bytes(wire)]
        signers = [{"inputs": list(range(len(inputs))), "keys": [str(lock_key)]}]
        signed = fixture.sign(label, tx, signers)
        if signed["hash"] != draft["hash"]:
            raise RuntimeError("Witness bytes changed the raw CKB transaction hash")
        return signed, [*deposits, change], len(wire)

    def commit(name, inputs, objs, **options):
        signed, amounts, group_bytes = build(name, inputs, objs, **options)
        measured = confirmed(l1, signed)
        results[name] = {**measured, "occupied_shannons": amounts[:len(objs)],
                         "group_witness_bytes": group_bytes}
        write_json(args.output / "successes.json", results)
        return [point(measured["transaction"], i) for i in range(len(amounts))], sum(amounts)

    def reject(name, code, inputs, objs, **options):
        signed, _, _ = build(name, inputs, objs, **options)
        reason = l1.reject(signed)
        if f"error code {code} " not in reason:
            raise RuntimeError(f"{name} failed for a different reason: {reason}")
        rejections[name] = reason
        write_json(args.output / "rejections.json", rejections)

    create = dict(action=1, recipient=fixture.owner_lock, lock_key=fixture.faucet, budget=funding)
    reject("missing-genesis", 41, [fee], objects, headers=[], **create)
    foreign = dict(physical, args=hex_bytes(b"\x81" * 32 + physical_args[32:]))
    reject("foreign-genesis", 41, [fee], objects, ty=foreign, **create)
    reject("forged-object-id", 46, [fee], [
        object_for(adapters, deployment_hash, fee, 7, keys[0], objects[0].state), objects[1],
    ], **create)
    issued, budget = commit("issue", [fee], objects, **create)
    transfer = dict(action=2, recipient=fixture.friend_lock, lock_key=fixture.owner, budget=budget)
    reject("missing-approval", 46, issued, objects, edit=lambda entries: b"", **transfer)
    reject("unknown-group", 47, issued, objects, edit=lambda entries: encode([
        (*entries[0][:1], b"\x91" * 32, *entries[0][2:]), entries[1],
    ]), **transfer)
    reject("duplicate-witness", 47, issued, objects, edit=lambda entries: encode([*entries, entries[0]]), **transfer)
    reject("noncanonical-order", 47, issued, objects, edit=lambda entries: encode(
        sorted(entries, key=lambda row: row[:2], reverse=True), canonical=False), **transfer)
    reject("crossed-seals", 46, issued, objects, edit=lambda entries: encode([
        (*entry[:-1], entries[1 - i][-1]) for i, entry in enumerate(entries)
    ]), **transfer)
    reject("forged-recipient", 46, issued, objects, edit=lambda entries: encode([
        (*entries[0][:3], b"\x92" * 32, *entries[0][4:]), entries[1],
    ]), **transfer)
    reject("forged-content-proof", 46, issued, objects, edit=lambda entries: encode([
        (*entries[0][:4], b"\x93" * 32, entries[0][-1]), entries[1],
    ]), **transfer)
    edited = contents[0][:-1] + bytes([contents[0][-1] ^ 1])
    changed = Object(objects[0].identity, objects[0].approval, keys[0], edited)
    reject("content-edit-with-authority", 46, issued, [changed, objects[1]], **transfer)
    reject("removed-approval-type", 46, issued, objects, data=[
        objects[0].manifest(omit_approval=True), objects[1].manifest(),
    ], edit=lambda entries: encode(entries[1:]), **transfer)
    reject("duplicate-object", 46, issued, objects, data=[
        objects[0].manifest(), objects[0].manifest(), objects[1].manifest(),
    ], **transfer)
    received, budget = commit("transfer", issued, objects, **transfer)
    refresh = dict(action=2, recipient=fixture.friend_lock, lock_key=fixture.friend, budget=budget)
    reject("replayed-authorizations", 46, received, objects,
           edit=lambda entries: encode(policy_entries["transfer"]), **refresh)
    refreshed, budget = commit("refresh", received, objects, **refresh)
    kept = results["refresh"]["occupied_shannons"][1]
    commit("single-object-transfer", [refreshed[0], refreshed[2]], [objects[0]],
           action=2, recipient=fixture.owner_lock, lock_key=fixture.friend, budget=budget - kept)

    return {"genesis": genesis_hash, "deployment": deployment, "type": physical,
            "contents": list(map(hex_bytes, contents)), "results": results, "rejections": rejections}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "dispatcher", "object", "approval", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--l1-port", type=int, default=30440)
    parser.add_argument("--port", type=int, default=30450)
    for name in ("native-lock", "native-object", "native-policy"):
        parser.add_argument("--" + name, type=Path)
    args = parser.parse_args()
    args.validators = 4
    native = (args.native_lock, args.native_object, args.native_policy)
    if any(native) and not all(native):
        parser.error("native differential requires all three Stem scripts")
    args.output.mkdir(parents=True)
    node = L1(args)
    try:
        fixture = Fixtures(args)
        report = exercise(args, node, fixture)
        write_json(args.output / "metatype-object-report.json", report)
    finally:
        node.stop()
    if all(native):
        differential = exercise_polytype(args, fixture, list(map(binary, report["contents"])), report)
        write_json(args.output / "polytype-differential.json", differential)
    print(f"Metatype object evidence written to {args.output}")


if __name__ == "__main__":
    main()

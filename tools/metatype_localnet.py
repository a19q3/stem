#!/usr/bin/env python3
"""Deploy and exercise Metatype and independent RISC-V adapters on CKB."""

import argparse
import time
from pathlib import Path

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes,
    le, output, packed_script, point, script, script_hash, transaction, wallet_group,
)
from localnet import write_json


def metatype_data(components):
    return le(len(components), 1) + b"".join(
        le(len(logical), 2) + logical + le(len(state), 2) + state
        for logical, state in sorted(components)
    )



def manifest_definition(components):
    return metatype_data([(logical, b"") for logical, _ in components])


def metatype_reference(components, definition=None):
    if definition is None:
        definition = manifest_definition(components)
    return b"\0" + ckb_hash(definition) + b"".join(
        le(len(state), 2) + state for _, state in sorted(components)
    )


def asset_state(asset, units, policies):
    return [(asset, le(units, 16)), *[(policy, b"") for policy in policies]]

def capacity(lock, ty, data):
    script_size = lambda s: 33 + len(binary(s["args"]))
    return (8 + script_size(lock) + script_size(ty) + len(data)) * CKB


def cycle_estimate(l1, signed):
    return int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)


def confirmed(l1, signed):
    cycles = cycle_estimate(l1, signed)
    start = time.monotonic()
    tx = l1.send(signed)
    elapsed_ms = (time.monotonic() - start) * 1000
    return {
        "transaction": tx, "cycles": cycles, "transaction_bytes": len(binary(signed["molecule"])),
        "witness_bytes": sum(len(binary(data)) for data in signed["json"]["witnesses"]),
        "submit_through_local_mining_ms": elapsed_ms, "inclusion": l1.prove_committed(tx),
    }


def exercise(args, l1, fixture):
    programs = [args.dispatcher.read_bytes(), args.udt.read_bytes(), args.policy.read_bytes()]
    dispatcher_code, udt_code, policy_code = [ckb_hash(code) for code in programs]
    table = bytes([2]) + b"".join(sorted([udt_code + b"\0" + udt_code, policy_code + b"\0" + policy_code])) + b"\0"
    genesis = l1.call("get_block_by_number", "0x0")
    faucet, system = genesis["transactions"][:2]
    faucet_index, faucet_cell = next(
        (i, out) for i, out in enumerate(faucet["outputs"]) if out["lock"]["args"] == FAUCET_ARG
    )
    secp = dependency(point(system["hash"], 0), "dep_group")
    deployed_data = programs + [table]
    deployment_cost = [(len(data) + 100) * CKB for data in deployed_data]
    funds = int(faucet_cell["capacity"], 16) - sum(deployment_cost) - FEE
    deployed = transaction(
        [point(faucet["hash"], faucet_index)],
        [output(amount, fixture.standard) for amount in [*deployment_cost, funds]],
        [hex_bytes(data) for data in deployed_data] + ["0x"], [secp],
    )
    deployment = confirmed(l1, fixture.sign("deploy", deployed, [wallet_group(0, [fixture.faucet])]))
    deploy_tx = deployment["transaction"]
    deps = [secp, *[dependency(point(deploy_tx, i)) for i in range(4)]]
    fund_point = point(deploy_tx, len(deployed_data))
    genesis_hash = binary(genesis["header"]["hash"])
    namespace = ckb_hash(b"Metatype localnet\0" + genesis_hash)
    physical = script(hex_bytes(dispatcher_code), hex_bytes(genesis_hash + namespace + ckb_hash(table)), "data2")
    foreign = dict(physical, args=hex_bytes(genesis_hash + ckb_hash(b"Metatype foreign\0" + namespace) + ckb_hash(table)))
    issuer, recipient = script_hash(fixture.standard), script_hash(fixture.friend_lock)
    rejections = {}
    encode_cell = metatype_reference if args.shared_manifests else metatype_data
    definitions, publications = {}, []
    expansion_limit = None
    current_deps = deps

    def publish_definitions(raw_definitions, *, replace=False):
        nonlocal funds, fund_point
        unique = {ckb_hash(data): data for data in raw_definitions}
        if not replace:
            unique = {key: data for key, data in unique.items() if key not in definitions}
        if not unique:
            return
        retired = [definitions[key]["out_point"] for key in unique] if replace else []
        occupied = [(61 + len(data)) * CKB for data in unique.values()]
        change = funds - FEE - (0 if replace else sum(occupied))
        inputs = [fund_point, *retired]
        tx = transaction(inputs,
                         [output(value, fixture.standard) for value in [*occupied, change]],
                         [hex_bytes(data) for data in unique.values()] + ["0x"], [secp])
        receipt = confirmed(l1, fixture.sign(f"definitions-{len(publications)}", tx,
            [dict(inputs=list(range(len(inputs))), keys=[str(fixture.faucet)])]))
        locations = []
        for index, (key, data) in enumerate(unique.items()):
            locations.append(dict(hash=hex_bytes(key), bytes=len(data), occupied_shannons=occupied[index]))
            definitions[key] = dependency(point(receipt["transaction"], index))
        for old in retired:
            assert l1.call("get_live_cell", old, False)["status"] != "live"
        publications.append(dict(receipt=receipt, definitions=locations, spent=retired))
        funds, fund_point = change, point(receipt["transaction"], len(occupied))

    def dependency_set(rows):
        hashes = sorted({data[1:33] for data in rows if data[:1] == b"\0"})
        return [*deps, *(definitions[key] for key in hashes)]

    def sign_spend(name, prior, rows, *, selected_deps=None, ty=physical, lock=fixture.friend_lock):
        inputs = [entry[0] for entry in prior]
        funds = sum(entry[1] for entry in prior)
        occupied = [capacity(lock, ty, data) for data in rows]
        change = funds - sum(occupied) - FEE
        if change < 61 * CKB:
            raise ValueError("Fixture did not fund occupied Capacity")
        tx = transaction(inputs,
                         [output(amount, lock, ty) for amount in occupied] + [output(change, fixture.friend_lock)],
                         [hex_bytes(data) for data in rows] + ["0x"],
                         current_deps if selected_deps is None else selected_deps)
        tx["header_deps"] = [hex_bytes(genesis_hash)]
        groups = [{"inputs": list(range(len(inputs))), "keys": [str(fixture.friend)]}]
        return fixture.sign(name, tx, groups), occupied

    def reject(name, expected, prior, rows, **overrides):
        rejected, _ = sign_spend(name, prior, rows, **overrides)
        assert_rejected(name, expected, rejected)

    def assert_rejected(name, expected, rejected):
        reason = l1.reject(rejected)
        if f"error code {expected} " not in reason:
            raise RuntimeError(f"{name} was rejected for a different reason: {reason}")
        rejections[name] = reason
        write_json(args.output / "rejections.json", rejections)

    samples, live = [], []
    for count in (2, 4, 8, 16):
        asset = packed_script(script(hex_bytes(udt_code), hex_bytes(issuer + bytes([count]) * 32), "data"))
        def policy(index):
            return packed_script(script(hex_bytes(policy_code), hex_bytes(
                ckb_hash(asset) + issuer + recipient + le(count * 100 + index, 32)), "data"))

        policies = [policy(index) for index in range(count - 1)]

        def row(units, constraints):
            return encode_cell(asset_state(asset, units, constraints))

        original = [row(25, policies), row(10, policies[:1])]
        full_definition = manifest_definition(asset_state(asset, 25, policies))
        if args.shared_manifests:
            publish_definitions([full_definition, manifest_definition(asset_state(asset, 10, policies[:1]))])
        current_deps = dependency_set(original)
        occupied = [capacity(fixture.friend_lock, physical, data) for data in original]
        amounts = [bond + 2000 * CKB for bond in occupied]
        funds -= sum(amounts) + FEE
        mint = transaction([fund_point],
                           [output(amount, fixture.friend_lock, physical) for amount in amounts]
                           + [output(funds, fixture.standard)],
                           [hex_bytes(data) for data in original] + ["0x"], current_deps)
        mint["header_deps"] = [hex_bytes(genesis_hash)]
        minted = confirmed(l1, fixture.sign(f"mint-{count}", mint, [wallet_group(0, [fixture.faucet])]))
        prior = [(point(minted["transaction"], i), value) for i, value in enumerate(amounts)]
        fund_point = point(minted["transaction"], len(amounts))

        next_data = [row(12, policies), row(13, policies), row(10, policies[:1])]
        if count == 4:
            def inline(units, constraints):
                return metatype_data(asset_state(asset, units, constraints))

            extra_deps = current_deps[len(deps):]
            reject("remove-input-policy", 46, prior, [inline(12, policies[:-1]), *next_data[1:]])
            reject("duplicate-policy", 43, prior, [inline(12, policies + [policies[0]]), *next_data[1:]])
            reject("missing-adapter", 44, prior, next_data, selected_deps=[*deps[:3], deps[-1], *extra_deps])
            reject("missing-table", 42, prior, next_data, selected_deps=[*deps[:-1], *extra_deps])
            reject("change-deployment", 46, prior, next_data, ty=foreign)
            reject("unauthorized-recipient", 46, prior, next_data, lock=fixture.owner_lock)
            reject("replace-input-policy", 46, prior, [
                inline(12, policies[:-1] + [policy(10)]), *next_data[1:],
            ])
            unknown = packed_script(script(hex_bytes(ckb_hash(b"unknown adapter")), kind="data"))
            reject("unknown-code", 44, prior, [inline(12, policies + [unknown]), *next_data[1:]])
            reject("inflate-supply", 46, prior, [row(13, policies), *next_data[1:]])
            reject("exceed-manifest-state-budget", 43, prior, [
                metatype_data([(asset, b"\0" * 2049)]), *next_data[1:],
            ])
            if args.shared_manifests:
                reject("missing-manifest-definition", 43, prior, next_data, selected_deps=deps)
                reject("truncated-shared-state", 43, prior, [next_data[0][:-1], *next_data[1:]])
                reject("extra-shared-state", 43, prior, [next_data[0] + b"\0\0", *next_data[1:]])
                substituted = next_data[0][:1] + ckb_hash(b"not a supplied definition") + next_data[0][33:]
                reject("unavailable-manifest-hash", 43, prior, [substituted, *next_data[1:]])
                bad_definitions = [inline(12, policies), next_data[0],
                                   manifest_definition(asset_state(asset, 12, policies + [policies[0]]))]
                reduced = asset_state(asset, 12, policies[:-1])
                publish_definitions([*bad_definitions, manifest_definition(reduced)])
                for name, invalid in zip(("nonempty-definition-state", "nested-manifest-definition",
                                          "duplicate-definition-script"), bad_definitions, strict=True):
                    malformed = metatype_reference(asset_state(asset, 12, policies), invalid)
                    reject(name, 43, prior, [malformed, *next_data[1:]],
                           selected_deps=dependency_set([*original, *next_data, malformed]))
                weakened = metatype_reference(reduced)
                reject("replace-shared-script-set", 46, prior, [weakened, *next_data[1:]],
                       selected_deps=dependency_set([*original, *next_data, weakened]))
                publish_definitions([full_definition], replace=True)
                current_deps = dependency_set([*original, *next_data])
        if count == 16 and args.shared_manifests:
            filled = [(logical, b"x" * 256) for logical, _ in asset_state(asset, 12, policies)]
            expanded = len(metatype_data(filled))
            compact = metatype_reference(filled)
            copies = (128 * 1024) // expanded + 1
            assert len(compact) * copies <= 128 * 1024 < expanded * copies and copies <= 64
            occupied_capacity = capacity(fixture.friend_lock, physical, compact)
            oversized = transaction([fund_point],
                [output(occupied_capacity, fixture.friend_lock, physical)] * copies
                + [output(funds - copies * occupied_capacity - FEE, fixture.standard)],
                [hex_bytes(compact)] * copies + ["0x"], dependency_set([compact]))
            oversized["header_deps"] = [hex_bytes(genesis_hash)]
            assert_rejected("exceed-expanded-transaction-budget", 43,
                fixture.sign("expanded-over-budget", oversized, [wallet_group(0, [fixture.faucet])]))
            expansion_limit = {"cells": copies, "encoded_bytes": copies * len(compact),
                               "expanded_bytes": copies * expanded}
        signed, transfer_occupied = sign_spend(f"split-{count}", prior, next_data)
        transfer = confirmed(l1, signed)
        samples.append({"constraints": count, "manifest_bytes": list(map(len, original)),
                        "mint_occupied_shannons": occupied, "split_occupied_shannons": transfer_occupied,
                        "mint": minted, "split": transfer})
        live.append((transfer["transaction"], transfer_occupied, next_data, asset, policies))

    # Spend both assets together and put part of each in the same output.
    inputs, input_data, data, combined = [], [], [], []
    for tx, amounts, rows, asset, policies in live[:2]:
        # Keep the last output and fee change live. Combine both policy-bearing
        # portions of each asset into a single atomic transaction.
        for index in range(2):
            inputs.append((point(tx, index), amounts[index]))
            input_data.append(rows[index])
        combined.extend(asset_state(asset, 12, policies))
        # One inline output shares the group with the compact inputs and output.
        data.append(metatype_data(asset_state(asset, 13, policies)) if args.shared_manifests else rows[1])
        fee_index = len(rows)
        fee_change = l1.call("get_live_cell", point(tx, fee_index), False)["cell"]["output"]["capacity"]
        inputs.append((point(tx, fee_index), int(fee_change, 16)))
    if args.shared_manifests:
        publish_definitions([manifest_definition(combined)])
    composite_rows = [encode_cell(combined), *data]
    current_deps = dependency_set([*input_data, *composite_rows])
    composite, _ = sign_spend("atomic-two-assets", inputs, composite_rows)
    joint = confirmed(l1, composite)
    return {
        "ckb_genesis": genesis["header"]["hash"], "deployment": deployment, "type": physical,
        "adapter_table": hex_bytes(table), "samples": samples, "two_assets": joint, "rejections": rejections,
        "shared_manifests": args.shared_manifests, "definition_publications": publications,
        "expansion_limit": expansion_limit,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "dispatcher", "udt", "policy", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--l1-port", type=int, default=30420)
    parser.add_argument("--shared-manifests", action="store_true")
    args = parser.parse_args()
    args.output.mkdir(parents=True)
    node = L1(args)
    try:
        result = exercise(args, node, Fixtures(args))
        write_json(args.output / "metatype-report.json", result)
        print(f"Metatype CKB evidence written to {args.output}")
    finally:
        node.stop()


if __name__ == "__main__":
    main()

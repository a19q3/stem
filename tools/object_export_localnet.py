"""Export a native object, enforce its Type Scripts on CKB and return it."""

import json
import subprocess
import time

from bridge_localnet import (CKB, FEE, binary, ckb_hash, dependency, hex_bytes, l1_reference_request, le, output,
                             packed_script, point, script, script_hash, transaction, wallet_group)
from localnet import run, write_json
from metatype_localnet import (
    capacity as ckb_occupied, manifest_definition, metatype_data, metatype_reference,
)
from native_bridge_localnet import cell, signing_cell
from spore_settlement_localnet import molecule_bytes, table
from stake_localnet import witness


def object_round(args, l1, fixture, vault, spec, spec_path, mint, clients, relay):
    client = clients[0]
    network, genesis = client.network, vault["genesis"]
    empty = bytes(32)
    owner_key, buyer_key, authority_key = fixture.owner, fixture.friend, fixture.l1_keys[0]
    ckb_authority = dict(fixture.standard, args=fixture.key(authority_key)["blake160"])
    authority_id = script_hash(ckb_authority)
    native_code = hex_bytes(ckb_hash(args.native_lock.read_bytes()))
    native_scripts = [script(native_code, fixture.key(key)["blake160"], "data")
                      for key in (owner_key, buyer_key, authority_key)]
    owner, buyer, authority = native_scripts
    metatype = args.metatype_dispatcher is not None
    shared_manifest = args.metatype_shared_manifest
    revisable = args.metatype_revisable_object
    topup = args.metatype_revision_topup
    live_rule = args.metatype_rule_registry is not None
    append_only = args.native_object_append_only is not None
    state_limit = (700 if topup else 120) if args.native_object_state_limit else None
    rule_records = [le(state_limit, 2), le(state_limit + 200, 2)] if state_limit else []
    next_limit = 200 if live_rule else state_limit
    profile = bytes([3 if revisable else 1])
    mailbox_bin = args.intercell_mailbox.read_bytes()
    gate_bin = (args.metatype_mailbox_lock if metatype else args.ckb_stem_object).read_bytes()
    mailbox_code, ckb_code = map(ckb_hash, (mailbox_bin, gate_bin))
    native_object = hex_bytes(ckb_hash(args.stem_native_object.read_bytes()))
    policy_code = hex_bytes(ckb_hash(args.object_policy.read_bytes()))
    secp = vault["secp"]
    mailbox_cap = 1_000 * CKB
    funded_mailbox_cap = 3_300 * CKB if revisable else mailbox_cap
    authority_funds = (5_000 if revisable else 2_500 if metatype else 1_200) * CKB
    token_cap = (1_200 if topup else 1_000 if live_rule else 900 if metatype else 360) * CKB
    codes = [mailbox_bin, gate_bin]
    if metatype:
        codes.extend(path.read_bytes() for path in (args.metatype_dispatcher,
                     args.metatype_bridged_object, args.metatype_bridged_approval))
    if state_limit:
        codes.append(args.metatype_object_state_limit.read_bytes())
    if append_only:
        append_adapter = args.metatype_object_append_only.read_bytes()
        codes.append(append_adapter)
    if live_rule:
        codes.append(args.metatype_rule_registry.read_bytes())
    ckb_items = codes + ([b""] if live_rule else rule_records)
    mailbox_seed_index = len(ckb_items)
    auth_index, change_index = mailbox_seed_index + 1, mailbox_seed_index + 2
    item_caps = [(len(item) + 100) * CKB for item in ckb_items]
    if live_rule:
        item_caps[-1] = 800 * CKB
    funding_change = vault["change"] - sum(item_caps) - mailbox_cap - authority_funds - 2 * FEE
    funding = transaction([vault["change_point"]],
        [output(cap, fixture.standard) for cap in item_caps] +
        [output(mailbox_cap + FEE, fixture.standard), output(authority_funds, ckb_authority),
         output(funding_change, fixture.standard)],
        [hex_bytes(item) for item in ckb_items] + ["0x"] * 3, [secp])
    funded = l1.send(fixture.sign("object-fund-ckb", funding, [wallet_group(0, [fixture.faucet])]))
    type_id = ckb_hash(le(0, 8) + binary(funded) + le(mailbox_seed_index, 4) + le(0, 8))
    anchor_code, anchor_id = binary(vault["anchor"]["hash"]), vault["anchor"]["id"]
    ckb_deps = [secp, *[dependency(point(funded, i)) for i in range(len(codes))]]
    native_deps = [mint["lock_dep"], point(mint["mint_id"], 2), point(mint["mint_id"], 3)]
    authority_point = point(funded, auth_index)
    for index in range(sum((metatype, state_limit is not None, append_only))):
        native_deps.append(point(mint["mint_id"], 4 + index))
    if state_limit and not live_rule:
        l1_rule_point = point(funded, len(codes))
        l1_rule_dep = dependency(l1_rule_point)
        l1_other_rule_dep = dependency(point(funded, len(codes) + 1))
        ckb_deps.append(l1_rule_dep)

    def ckb_submit(signed):
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        start = time.monotonic()
        tx = l1.send(signed)
        committed_ms = (time.monotonic() - start) * 1000
        return {"hash": tx, "proof": l1.prove_committed(tx), "cycles": cycles,
                "bytes": len(binary(signed["molecule"])),
                "raw_witness_bytes": sum(len(binary(item)) for item in signed["json"]["witnesses"]),
                "submit_through_local_mining_ms": committed_ms}

    def reject_ckb(signed, *codes):
        result = l1.reject(signed)
        assert any(f"error code {code} " in result for code in codes), result
        return result

    def packed(point_):
        return binary(point_["tx_hash"]) + le(int(point_["index"], 16), 4)

    registry_rejections = {}
    if live_rule:
        rule_seed = point(funded, len(codes))
        registry_code = hex_bytes(ckb_hash(codes[-1]))
        rule_types = [script(registry_code, hex_bytes(ckb_hash(le(0, 8) + packed(rule_seed) + le(i, 8))),
                             "data2") for i in range(2)]
        rule_cap = 350 * CKB
        birth = transaction([rule_seed], [output(rule_cap, fixture.standard, ty) for ty in rule_types]
                            + [output(item_caps[-1] - 2 * rule_cap - FEE, fixture.standard)],
                            [hex_bytes(raw) for raw in rule_records] + ["0x"], ckb_deps)
        forged = transaction([rule_seed], [output(rule_cap, fixture.standard, rule_types[1])],
                             [hex_bytes(rule_records[0])], ckb_deps)
        registry_rejections["wrong_type_id_birth"] = reject_ckb(
            fixture.sign("object-false-rule-registry", forged, [wallet_group(0, [fixture.faucet])]), 57)
        registry_birth = ckb_submit(fixture.sign("object-rule-registry", birth, [wallet_group(0, [fixture.faucet])]))
        l1_rule_point = point(registry_birth["hash"], 0)
        l1_rule_dep = dependency(l1_rule_point)
        other_rule_point = point(registry_birth["hash"], 1)
        l1_other_rule_dep = dependency(other_rule_point)
        ckb_deps.append(l1_rule_dep)

    live_votes = None

    def native_sign(label, spending, outputs, *, endpoint=None, message=None, receive=False, votes=None, deps=None):
        locks = sorted({packed_script(lock): (lock, key) for _, lock, key in spending}.values(),
                       key=lambda pair: packed_script(pair[0]))
        points = [p for p, _, _ in spending]
        deps = native_deps if deps is None else deps
        request = {"inputs": [{"out_point": p} for p in points], "outputs": outputs,
                   "cell_deps": [dependency(p) for p in deps]}
        if message:
            group = next(i for i, (lock, _) in enumerate(locks) if lock == endpoint)
            request["messages"] = [{"group_index": group, "actions": [
                {"kind": 0xff01 if receive else 0xff00, "data": message["record"]}]}]
        path = args.output / f"object-{label}-request.json"
        draft = args.output / f"object-{label}-draft.json"
        context = args.output / f"object-{label}-context.json"
        write_json(path, request)
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", path)))
        observations = votes if votes is not None else live_votes
        if observations:
            observation_file = args.output / f"object-{label}-references.json"
            write_json(observation_file, observations)
            attached = args.output / f"object-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references", "--genesis", spec_path,
                "--transaction", draft, "--observations", observation_file)))
            draft = attached
        write_json(context, {"cells": [signing_cell(client, p) for p in points + deps]})
        for index, (lock, key) in enumerate(locks):
            if key is None:
                continue
            lock_path = args.output / f"object-{label}-lock-{index}.json"
            signed = args.output / f"object-{label}-group-{index}.json"
            write_json(lock_path, lock)
            write_json(signed, json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
                "--transaction", draft, "--context", context, "--group-index", str(index),
                "--lock", lock_path, "--key-file", key)))
            draft = signed
        encoded = json.loads(draft.read_text())
        write_json(args.output / f"object-{label}-stem-signed.json", encoded)
        return encoded

    def commit(tx):
        result = client.call("broadcast_tx_commit", tx=tx)
        assert result["check_tx"]["code"] == 0, result
        return result

    def reject_stem(tx, *causes):
        result = client.call("broadcast_tx_sync", tx=tx)
        assert result["code"] == 1 and any(cause in result["log"] for cause in causes), result
        return result["log"]

    def at(tx, index):
        return point("0x" + tx["hash"], index)

    def query(path, data="0x"):
        reply = client.query(path, data=data, prove=True)
        return reply["height"], json.loads(bytes.fromhex(reply["value"]))

    def attest(command, label, params):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            votes.append(json.loads(run(args.binary, "bridge", command, "--genesis", spec_path,
                "--l1-rpc", l1.url, "--key-file", key, "--journal", args.output / f"object-{label}-{index}", *params)))
        write_json(args.output / f"object-{label}-approvals.json", votes)
        return votes

    def attest_anchor(label, header):
        previous = query("/stem/bridge-anchor")[1]
        height = int(header["number"], 16)
        approvals = attest("attest-anchor", label, ["--previous-height", str(previous["height"]),
            "--previous-hash", previous["hash"], "--height", str(height)])
        approval_file = args.output / f"object-{label}-approvals.json"
        return commit(json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", approval_file)))

    def attest_references(label, header, *targets):
        request = args.output / f"object-{label}-reference-request.json"
        write_json(request, l1_reference_request(int(header["number"], 16), header["hash"], *targets))
        return attest("attest-reference", label, ["--request", request])

    seed_cap, auth_cap, object_cap, bond_cap = [capacity * CKB for capacity in (450, 200, 200, 120)]
    if topup:
        object_cap = (9 if state_limit else 8) * CKB
    return_topup = 5 * CKB if topup else 0
    native_rules = [cell(100 * CKB, owner, raw) for raw in rule_records] if not live_rule else []
    remainder = mint["payment_capacity"] - seed_cap - auth_cap - len(native_rules) * 100 * CKB - FEE
    separated = commit(native_sign("split", [(mint["payment"], owner, owner_key)],
                                   [cell(seed_cap, owner), cell(auth_cap, authority), cell(remainder, owner),
                                    *native_rules]))
    seed, native_authority, native_fee = [at(separated, i) for i in range(3)]
    object_id = ckb_hash(b"Stem object identity\0" + binary(network) + packed(seed) + le(0, 4))
    identity = script(native_object, hex_bytes(object_id + script_hash(authority)
        + authority_id + binary(policy_code) + (b"\x01" if revisable else b"")), "data")
    identity_hash = script_hash(identity)
    policy = script(policy_code, hex_bytes(identity_hash + script_hash(authority)), "data")
    types = [identity, policy]
    if append_only:
        append_code = ckb_hash(args.native_object_append_only.read_bytes())
        append_script = script(hex_bytes(append_code), hex_bytes(identity_hash), "data")
        types.append(append_script)
    if state_limit:
        rule_hash = script_hash(rule_types[0]) if live_rule else ckb_hash(rule_records[0])
        substitute_hash = script_hash(rule_types[1]) if live_rule else ckb_hash(rule_records[1])
        rule_tag = bytes([int(live_rule)])
        state_limit_code = ckb_hash(args.native_object_state_limit.read_bytes())
        types.append(script(hex_bytes(state_limit_code), hex_bytes(identity_hash + rule_tag + rule_hash), "data"))
        if live_rule:
            birth_header = registry_birth["proof"]["header"]
            l1.mine(spec["bridge"]["confirmations"])
            first_rule_anchor = attest_anchor("rule-anchor-initial", birth_header)
            initial_votes = attest_references("rule-initial", birth_header, (l1_rule_point, True))
            other_votes = attest_references("rule-other", birth_header, (other_rule_point, True))
            historical_votes = attest_references("rule-historical", birth_header, (l1_rule_point, False))
            live_votes = initial_votes
        else:
            native_rule_point, native_other_rule_point = [at(separated, i) for i in (3, 4)]
            native_deps.append(native_rule_point)
    def object_data(content, revision=0, mime=b"text/plain"):
        raw = table(molecule_bytes(mime), molecule_bytes(content), b"")
        return le(revision, 8) + raw if revisable else raw

    initial_content = b"Stem object" if revisable else b"Stem object with two Type Groups"
    stem_content = initial_content + b"; revised on Stem"
    ckb_content = stem_content + b"; transferred on CKB"
    data = object_data(initial_content)
    append_probes = {
        "rewrite": object_data(b"X" + stem_content[1:] + b".", 2),
        "truncate": object_data(stem_content[:-1], 2),
        "unchanged_content": object_data(stem_content, 2),
        "mime_change": object_data(stem_content + b".", 2, b"application/octet-stream"),
    } if append_only else {}
    revisions = {}
    definition = None
    definition_dep = None
    shared_evidence = None

    def projected(contents, selected=None):
        if not metatype:
            return contents
        selected = types if selected is None else selected
        components = [(packed_script(item), contents if item == identity else b"") for item in selected]
        if definition is not None and selected == types:
            return metatype_reference(components, definition)
        return metatype_data(components)

    if metatype:
        script_ids = b"".join(ckb_hash(item) for item in sorted(map(packed_script, types)))
        manifest = ckb_hash(b"Stem Metatype manifest\0" + script_ids)
        custody = mailbox_code + type_id + ckb_code + manifest + identity_hash
        adapter_entries = [binary(native_object) + b"\0" + ckb_hash(codes[3]),
                           binary(policy_code) + b"\0" + ckb_hash(codes[4])]
        if state_limit:
            adapter_entries.append(state_limit_code + b"\0" + ckb_hash(codes[5]))
        if append_only:
            adapter_entries.append(append_code + b"\0" + ckb_hash(append_adapter))
        adapter_table = bytes([len(adapter_entries)]) + b"".join(sorted(adapter_entries)) + profile + custody
        table_capacity = (len(adapter_table) + 100) * CKB
        table_outputs = [output(table_capacity, fixture.standard)]
        table_data = [hex_bytes(adapter_table)]
        table_change = funding_change - table_capacity - FEE
        if shared_manifest:
            definition = manifest_definition([(packed_script(item), b"") for item in types])
            definition_capacity = (61 + len(definition)) * CKB
            table_outputs.append(output(definition_capacity, fixture.standard))
            table_data.append(hex_bytes(definition))
            table_change -= definition_capacity
        fee_index = len(table_outputs)
        table_tx = transaction([point(funded, change_index)],
            [*table_outputs, output(table_change, fixture.standard)], [*table_data, "0x"], [secp])
        table_deployed = l1.send(fixture.sign("object-adapter-table", table_tx, [wallet_group(0, [fixture.faucet])]))
        ckb_deps.append(dependency(point(table_deployed, 0)))
        if shared_manifest:
            definition_dep = dependency(point(table_deployed, 1))
            ckb_deps.append(definition_dep)
            shared_evidence = {"hash": hex_bytes(ckb_hash(definition)), "bytes": len(definition),
                               "occupied_shannons": definition_capacity, "cell_dep": definition_dep,
                               "proof": l1.prove_committed(table_deployed)}
        wrapper = script(hex_bytes(ckb_hash(codes[2])), hex_bytes(binary(genesis)
            + ckb_hash(b"Stem Metatype local fixture\0" + identity_hash) + ckb_hash(adapter_table)), "data2")
        wrapper_hash = script_hash(wrapper)
        endpoint = script(hex_bytes(ckb_hash(args.stem_metatype_escrow.read_bytes())),
            hex_bytes(binary(genesis) + wrapper_hash + anchor_code + anchor_id + profile + custody), "data")
        endpoint_id = script_hash(endpoint)
        ckb_owner = script(hex_bytes(ckb_code), hex_bytes(profile + endpoint_id + wrapper_hash
            + mailbox_code + type_id + manifest + identity_hash), "data2")
    else:
        manifest = identity_hash
        endpoint = script(native_object, hex_bytes(identity_hash + binary(genesis) + mailbox_code
            + type_id + ckb_code + anchor_code + anchor_id + authority_id), "data")
        endpoint_id = script_hash(endpoint)
        ckb_owner = script(hex_bytes(ckb_code), hex_bytes(type_id + endpoint_id + identity_hash
            + mailbox_code + authority_id), "data2")
    mailbox = script(hex_bytes(mailbox_code), hex_bytes(type_id + binary(genesis) + binary(network)
        + script_hash(ckb_owner) + anchor_code + anchor_id), "data2")
    mailbox_id = script_hash(mailbox)
    if not metatype:
        wrapper = script(hex_bytes(ckb_code), hex_bytes(mailbox_id + script_hash(ckb_owner) + authority_id), "data2")
    initial = transaction([point(funded, mailbox_seed_index)], [output(mailbox_cap, ckb_owner, mailbox)],
                          [hex_bytes(le(0, 8) + empty)], ckb_deps)
    initial["header_deps"] = [genesis]
    live_mailbox = point(l1.send(fixture.sign("object-create-mailbox", initial,
        [wallet_group(0, [fixture.faucet])])), 0)

    def packet(label, from_stem, recipient, nonce, content=None, scripts=manifest):
        if content is None:
            content = data
        source, destination, sender, receiver = ((network, genesis, endpoint_id, mailbox_id) if from_stem
                                                 else (genesis, network, mailbox_id, endpoint_id))
        request = args.output / f"object-{label}-message.json"
        write_json(request, dict(source=source, destination=destination, sender=hex_bytes(sender),
            receiver=hex_bytes(receiver), nonce=nonce, payload=hex_bytes(scripts + recipient + content)))
        return json.loads(run(args.binary, "intercell", "encode", "--request", request))

    rejected = {}
    auth_input = (native_authority, authority, authority_key)
    issued_outputs = [cell(object_cap, owner, data, types), cell(bond_cap, endpoint),
                      cell(seed_cap - object_cap - bond_cap - FEE, owner), cell(auth_cap, authority)]
    stripped_issue = [cell(object_cap, owner, data, [identity]), *issued_outputs[1:]]
    rejected["stem_mint_without_policy_type"] = reject_stem(native_sign("missing-policy",
        [(seed, owner, owner_key), auth_input], stripped_issue), "exited with 117")
    rejected["stem_mint_without_approval"] = reject_stem(native_sign("no-creation-approval",
        [(seed, owner, owner_key)], issued_outputs[:3]), "exited with 115")
    wrong = dict(identity, args=hex_bytes(bytes([7]) * 32 + binary(identity["args"])[32:]))
    wrong_policy = dict(policy, args=hex_bytes(script_hash(wrong) + script_hash(authority)))
    rejected["stem_forged_id"] = reject_stem(native_sign("forged-object", [(seed, owner, owner_key), auth_input],
        [cell(object_cap, owner, data, [wrong, wrong_policy]), *issued_outputs[1:]]), "exited with 117")
    if revisable:
        revision_errors = ("exited with 117", "exited with 58") if append_only else ("exited with 117",)
        rejected["stem_mint_at_nonzero_revision"] = reject_stem(native_sign("nonzero-revision",
            [(seed, owner, owner_key), auth_input],
            [cell(object_cap, owner, object_data(b"nonzero revision", 1), types), *issued_outputs[1:]]),
            *revision_errors)
    issued = commit(native_sign("issue", [(seed, owner, owner_key), auth_input], issued_outputs))
    wrapped_input = at(issued, 0)
    bond = at(issued, 1)
    native_authority = at(issued, 3)
    native_auth_capacity = auth_cap
    if revisable:
        previous = [(wrapped_input, owner, owner_key), (native_authority, authority, authority_key)]
        native_update = object_data(stem_content, 1)
        cheaper_auth = cell(auth_cap - FEE, authority)
        rejected["stem_edit_without_revision"] = reject_stem(native_sign("edit-without-revision", previous,
            [cell(object_cap, owner, object_data(b"edited under revision zero"), types), cheaper_auth]),
            *revision_errors)
        if state_limit:
            valid_update = [cell(object_cap, owner, native_update, types), cheaper_auth]
            absent = dict(votes=[]) if live_rule else dict(deps=native_deps[:-1])
            unrelated = (dict(votes=other_votes) if live_rule else
                         dict(deps=[*native_deps[:-1], native_other_rule_point]))
            rejected["stem_revision_missing_external_rule"] = reject_stem(native_sign("missing-external-rule", previous,
                valid_update, **absent), "exited with 56")
            rejected["stem_revision_substituted_external_rule"] = reject_stem(native_sign("substitute-external-rule", previous,
                valid_update, **unrelated), "exited with 56")
            if live_rule:
                rejected["stem_historical_rule"] = reject_stem(native_sign("historical-rule", previous,
                    valid_update, votes=historical_votes), "exited with 56")
            rejected["stem_revision_removes_state_limit"] = reject_stem(native_sign("remove-state-limit", previous,
                [cell(object_cap, owner, native_update, types[:-1]), cheaper_auth]), "exited with 56")
            substitute = dict(types[-1], args=hex_bytes(identity_hash + rule_tag + substitute_hash))
            both = (dict(votes=attest_references("both-rules", birth_header,
                                                (l1_rule_point, True), (other_rule_point, True))) if live_rule else
                    dict(deps=[*native_deps, native_other_rule_point]))
            rejected["stem_revision_replaces_state_limit"] = reject_stem(native_sign("replace-state-limit", previous,
                [cell(object_cap, owner, native_update, [*types[:-1], substitute]), cheaper_auth],
                **both), "exited with 56")
        if append_only:
            retained = [item for item in types if item != append_script]
            rejected["stem_revision_removes_append_only"] = reject_stem(native_sign("remove-append-only", previous,
                [cell(object_cap, owner, native_update, retained), cheaper_auth]), "exited with 58")
        revised = commit(native_sign("revise-in-stem", previous,
            [cell(object_cap, owner, native_update, types), cheaper_auth]))
        revisions["native"] = revised
        wrapped_input, native_authority = [at(revised, i) for i in range(2)]
        data = native_update
        native_auth_capacity -= FEE
        previous = [(wrapped_input, owner, owner_key), (native_authority, authority, authority_key)]
        for label, invalid in append_probes.items():
            rejected[f"stem_append_{label}"] = reject_stem(native_sign(f"append-{label}", previous,
                [cell(object_cap, owner, invalid, types), cell(native_auth_capacity - FEE, authority)]),
                "exited with 58")
        if state_limit:
            over_limit = object_data(stem_content.ljust(state_limit, b"x"), 2)
            rejected["stem_revision_exceeds_external_limit"] = reject_stem(native_sign("external-rule-limit", previous,
                [cell(object_cap + return_topup, owner, over_limit, types),
                 cell(native_auth_capacity - FEE - return_topup, authority)]),
                "exited with 56")
    nonce_domain = b"Stem Metatype escrow nonce\0" if metatype else b"Stem Spore return nonce\0"
    out_nonce = int.from_bytes(ckb_hash(nonce_domain + packed(bond))[:8], "little")
    outbound = packet("export", True, script_hash(fixture.owner_lock), out_nonce)
    route_inputs = [(bond, endpoint, None), (wrapped_input, owner, owner_key)]
    route_outputs = [cell(bond_cap, endpoint), cell(object_cap, endpoint, data, types)]
    rejected["stem_export_without_approval"] = reject_stem(native_sign("no-export-approval",
        route_inputs + [(native_fee, owner, owner_key)], route_outputs + [cell(remainder - FEE, owner)],
        endpoint=endpoint, message=outbound), "exited with 115")
    escrow_invalid = 53 if metatype else 118
    rejected["stem_export_to_mailbox_lock"] = reject_stem(native_sign("export-to-mailbox",
        route_inputs + [(native_authority, authority, authority_key)],
        route_outputs + [cell(native_auth_capacity - FEE, authority)], endpoint=endpoint,
        message=packet("export-to-mailbox", True, script_hash(ckb_owner), out_nonce)), f"exited with {escrow_invalid}")
    if metatype:
        wrong_manifest = bytes([1]) * 32
        rejected["stem_export_wrong_manifest"] = reject_stem(native_sign("export-wrong-manifest",
            route_inputs + [(native_authority, authority, authority_key)],
            route_outputs + [cell(native_auth_capacity - FEE, authority)], endpoint=endpoint,
            message=packet("export-wrong-manifest", True, script_hash(fixture.owner_lock),
                           out_nonce, scripts=wrong_manifest)), "exited with 53")
    else:
        extra = script(policy_code, hex_bytes(identity_hash + script_hash(owner)), "data")
        rejected["stem_dedicated_export_extra_constraint"] = reject_stem(native_sign("dedicated-extra-constraint",
            route_inputs + [(native_authority, authority, authority_key)],
            [route_outputs[0], cell(object_cap, endpoint, data, [*types, extra]),
             cell(native_auth_capacity - FEE, authority)], endpoint=endpoint, message=outbound),
            "exited with 119")
    if revisable:
        forged = object_data(stem_content + b"; edit in escrow", 2)
        rejected["stem_export_changes_revision"] = reject_stem(native_sign("export-revising",
            route_inputs + [(native_authority, authority, authority_key)],
            [route_outputs[0], cell(object_cap, endpoint, forged, types),
             cell(native_auth_capacity - FEE, authority)],
            endpoint=endpoint, message=packet("export-revising", True, script_hash(fixture.owner_lock),
                                              out_nonce, content=forged)),
            "exited with 54")
    exported = commit(native_sign("export", route_inputs + [(native_authority, authority, authority_key)],
        route_outputs + [cell(native_auth_capacity - FEE, authority)], endpoint=endpoint, message=outbound))
    bond, original, native_authority = [at(exported, i) for i in range(3)]
    def certified_cell(outpoint):
        point_bytes = hex_bytes(packed(outpoint))
        height, path = query("/stem/cell", point_bytes)
        return {**path, "point": point_bytes, "height": height, "finality": client.finality(height)["proof"]}

    escrowed = certified_cell(original)
    assert escrowed["cell"] and escrowed["proof"]
    assert signing_cell(client, original)["cell"] == route_outputs[1]

    proof_height, outbox = query("/stem/intercell/outbox", outbound["id"])
    checkpoint = relay.advance(proof_height, secp, probe=True)
    header, request = [args.output / f"object-outbox-{suffix}.json" for suffix in ("header", "request")]
    write_json(header, hex_bytes(relay.header(proof_height)))
    write_json(request, {**outbox, "consumed": [], "expected_root": hex_bytes(empty)})
    proof = json.loads(run(args.binary, "intercell", "delivery-witness", "--request", request, "--header", header))
    write_json(args.output / "object-ckb-witness.json", proof)
    root = binary(proof["successor_root"])
    path = binary(proof["input_type"])
    next_nonce = 0

    def mint_ckb(label, recipient=fixture.owner_lock, contents=data, *, include_checkpoint=True,
                 include_policy=True, output_type=wrapper, approve=True, include_definition=True,
                 fund_mailbox=True):
        input_point, amount, signer, fee_lock = (authority_point, authority_funds, authority_key, ckb_authority)
        if not approve:
            input_point, amount = point(table_deployed, fee_index), table_change
            signer, fee_lock = fixture.faucet, fixture.standard
        deps = ckb_deps if include_definition else [dep for dep in ckb_deps if dep != definition_dep]
        mailbox_output_cap = funded_mailbox_cap if fund_mailbox else mailbox_cap
        fee_change = amount - token_cap - (mailbox_output_cap - mailbox_cap) - FEE
        tx = transaction([live_mailbox, input_point], [output(mailbox_output_cap, ckb_owner, mailbox),
            output(token_cap, recipient, output_type), output(fee_change, fee_lock)],
            [hex_bytes(le(next_nonce, 8) + root), hex_bytes(projected(contents) if include_policy
                else metatype_data([(packed_script(identity), contents)])), "0x"],
            deps + ([checkpoint] if include_checkpoint else []))
        tx["witnesses"] = [witness(path)]
        if metatype:
            tx["header_deps"] = [genesis]
        return fixture.sign(f"object-{label}", tx, [wallet_group(1, [signer])])

    gate_invalid = 51 if metatype else 123
    rejected["ckb_mint_wrong_recipient"] = reject_ckb(mint_ckb("redirect-mint", recipient=fixture.friend_lock), gate_invalid)
    rejected["ckb_mint_changed_data"] = reject_ckb(mint_ckb("replace-content", contents=data[:-1] + b"!"), gate_invalid)
    rejected["ckb_mint_without_checkpoint"] = reject_ckb(mint_ckb("no-checkpoint", include_checkpoint=False), 84)
    if metatype:
        # A valid approval signature cannot mint in the pinned deployment on its own.
        spoof = transaction([authority_point],
            [output(token_cap, fixture.owner_lock, wrapper), output(authority_funds - token_cap - FEE, ckb_authority)],
            [hex_bytes(projected(data)), "0x"], ckb_deps)
        spoof["header_deps"] = [genesis]
        rejected["ckb_import_without_mailbox"] = reject_ckb(fixture.sign("object-no-mailbox", spoof,
            [wallet_group(0, [authority_key])]), 48)
        rejected["ckb_import_without_original_policy"] = reject_ckb(
            mint_ckb("omit-source-policy", include_policy=False), gate_invalid, 48)
        args_raw = binary(wrapper["args"])
        foreign = dict(wrapper, args=hex_bytes(args_raw[:32] + bytes([1]) * 32 + args_raw[64:]))
        rejected["ckb_import_foreign_deployment"] = reject_ckb(
            mint_ckb("foreign-deployment", output_type=foreign), gate_invalid, 48)
        rejected["ckb_import_without_approval"] = reject_ckb(mint_ckb("no-import-approval", approve=False), 46)
        if shared_manifest:
            rejected["ckb_import_without_manifest_definition"] = reject_ckb(
                mint_ckb("no-manifest-definition", include_definition=False), 43, gate_invalid)
    if revisable:
        rejected["ckb_import_unfunded_return_reserve"] = reject_ckb(
            mint_ckb("unfunded-return-reserve", fund_mailbox=False), gate_invalid)
    ckb_minted = ckb_submit(mint_ckb("mint"))
    live_mailbox, wrapped_point, authority_point = [point(ckb_minted["hash"], i) for i in range(3)]
    authority_funds -= token_cap + (funded_mailbox_cap - mailbox_cap) + FEE
    mailbox_cap = funded_mailbox_cap

    def transfer(label, authority_consent=True, include_policy=True, contents=data,
                 from_key=fixture.owner, *, selected=None, deps=None):
        inputs = [wrapped_point]
        outputs = [output(token_cap - (0 if authority_consent else FEE), fixture.friend_lock, wrapper)]
        keys = [wallet_group(0, [from_key])]
        body = [hex_bytes(projected(contents, selected if include_policy else [identity]))]
        if authority_consent:
            inputs.append(authority_point)
            outputs.append(output(authority_funds - FEE, ckb_authority))
            body.append("0x")
            keys.append(wallet_group(1, [authority_key]))
        tx = transaction(inputs, outputs, body, ckb_deps if deps is None else deps)
        if metatype:
            tx["header_deps"] = [genesis]
        return fixture.sign(f"object-{label}", tx, keys)

    approval_invalid = 46 if metatype else 121
    rejected["ckb_transfer_without_approval"] = reject_ckb(transfer("no-transfer-approval", False), approval_invalid)
    if metatype:
        rejected["ckb_transfer_policy_removed"] = reject_ckb(transfer("transfer-without-policy", include_policy=False), 48)
    return_state = data
    if revisable:
        ckb_update = object_data(ckb_content, 2)
        rejected["ckb_edit_without_revision"] = reject_ckb(
            transfer("edit-without-revision", contents=object_data(b"same revision tamper", 1)), 48)
        rejected["ckb_revision_without_approval"] = reject_ckb(
            transfer("revision-without-approval", False, contents=ckb_update), approval_invalid)
        for label, invalid in append_probes.items():
            rejected[f"ckb_append_{label}"] = reject_ckb(
                transfer(f"append-{label}", contents=invalid), 46)
        if append_only:
            retained = [item for item in types if item != append_script]
            rejected["ckb_revision_removes_append_only"] = reject_ckb(
                transfer("remove-append-only", contents=ckb_update, selected=retained), 48)
        if state_limit:
            without_rule = [dep for dep in ckb_deps if dep != l1_rule_dep]
            rejected["ckb_revision_exceeds_external_limit"] = reject_ckb(
                transfer("external-rule-limit", contents=over_limit), 46)
            rejected["ckb_revision_missing_external_rule"] = reject_ckb(
                transfer("missing-external-rule", contents=ckb_update, deps=without_rule), 46)
            rejected["ckb_revision_substituted_external_rule"] = reject_ckb(
                transfer("substitute-external-rule", contents=ckb_update, deps=[*without_rule, l1_other_rule_dep]), 46)
            rejected["ckb_revision_removes_state_limit"] = reject_ckb(
                transfer("remove-state-limit", contents=ckb_update, selected=types[:-1]), 48)
            rejected["ckb_revision_replaces_state_limit"] = reject_ckb(
                transfer("replace-state-limit", contents=ckb_update, selected=[*types[:-1], substitute],
                         deps=[*ckb_deps, l1_other_rule_dep]), 48)
        transferred = ckb_submit(transfer("transfer-and-revise", contents=ckb_update))
    else:
        transferred = ckb_submit(transfer("transfer"))
    wrapped_point, authority_point = [point(transferred["hash"], i) for i in range(2)]
    authority_funds -= FEE

    rule_evidence = None
    if state_limit:
        if live_rule:
            def rule_update(label, value, *, keep_type=True, lock=None):
                tx = transaction([l1_rule_point], [output(rule_cap - FEE, lock or fixture.standard,
                    rule_types[0] if keep_type else None)], [hex_bytes(le(value, 2))],
                    [secp, dependency(point(funded, len(codes) - 1))])
                return fixture.sign(f"object-{label}", tx, [wallet_group(0, [fixture.faucet])])

            registry_rejections["decrease"] = reject_ckb(rule_update("decrease-rule", state_limit - 1), 57)
            registry_rejections["unchanged"] = reject_ckb(rule_update("same-rule", state_limit), 57)
            registry_rejections["remove"] = reject_ckb(rule_update("remove-rule", next_limit, keep_type=False), 57)
            registry_rejections["owner_change"] = reject_ckb(rule_update("rule-owner-change", next_limit,
                                                                       lock=fixture.friend_lock), 57)
            republished = ckb_submit(rule_update("expand-rule", next_limit))
            rule_birth = registry_birth["proof"]
        else:
            rotate = transaction([l1_rule_point], [output(item_caps[len(codes)] - FEE, fixture.standard)],
                                 [hex_bytes(rule_records[0])], [secp])
            republished = ckb_submit(fixture.sign("object-republish-external-rule", rotate,
                                                 [wallet_group(0, [fixture.faucet])]))
            rule_birth = l1.prove_committed(funded)
        assert l1.call("get_live_cell", l1_rule_point, True)["status"] != "live"
        l1_rule_replacement = point(republished["hash"], 0)
        ckb_deps.remove(l1_rule_dep)
        ckb_deps.append(dependency(l1_rule_replacement))
        rule_evidence = {"bytes": hex_bytes(rule_records[0]),
                         "state_byte_limit": state_limit, "type": types[-1],
                         "differential_input": hex_bytes(data), "differential_rejected_output": hex_bytes(over_limit),
                         "ckb_initial_point": l1_rule_point, "ckb_initial_inclusion": rule_birth,
                         "ckb_replacement_point": l1_rule_replacement}
        if live_rule:
            rule_evidence.update(registry_type=rule_types[0], registry_hash=hex_bytes(rule_hash),
                                 next_limit=next_limit, updated_bytes=hex_bytes(le(next_limit, 2)), ckb_update=republished,
                                 first_rule_anchor=first_rule_anchor, registry_rejections=registry_rejections)
        else:
            rule_evidence.update(data_hash=hex_bytes(rule_hash), ckb_republication=republished,
                                 stem_initial=certified_cell(native_rule_point))

    if revisable:
        rejected["ckb_revision_rollback"] = reject_ckb(
            transfer("revision-rollback", contents=data, from_key=fixture.friend), 48)
        content = ckb_content + b"; return"
        if state_limit:
            content = content.ljust(next_limit - len(object_data(b"", 4)), b".")
        elif topup:
            content = content.ljust(630, b".")
        return_state = object_data(content, 4)
        if state_limit:
            assert len(return_state) == next_limit
        updated = ckb_submit(transfer("revise-on-ckb", contents=return_state, from_key=fixture.friend))
        revisions["ckb"] = updated
        wrapped_point, authority_point = [point(updated["hash"], i) for i in range(2)]
        authority_funds -= FEE

    inbound = packet("return", False, script_hash(buyer), 0, content=return_state)
    def return_ckb(label, *, consume=True, message=inbound, approve=True, deps=None):
        inputs = [live_mailbox]
        outputs = [output(mailbox_cap, ckb_owner, mailbox)]
        data_items = [hex_bytes(le(1, 8) + root + binary(message["record"]))]
        keys = []
        if consume:
            inputs.append(wrapped_point)
            outputs.append(output(token_cap - (0 if approve else FEE), fixture.friend_lock))
            data_items.append("0x")
            keys.append(wallet_group(1, [fixture.friend]))
        if approve:
            keys.append(wallet_group(len(inputs), [authority_key]))
            inputs.append(authority_point)
            outputs.append(output(authority_funds - FEE, ckb_authority))
            data_items.append("0x")
        tx = transaction(inputs, outputs, data_items, ckb_deps if deps is None else deps)
        if metatype and consume:
            tx["header_deps"] = [genesis]
        return fixture.sign(f"object-{label}", tx, keys)

    rejected["ckb_return_without_burn"] = reject_ckb(return_ckb("return-without-burn", consume=False), gate_invalid)
    rejected["ckb_return_to_escrow_lock"] = reject_ckb(return_ckb("return-to-escrow",
        message=packet("return-to-escrow", False, endpoint_id, 0)), gate_invalid)
    if metatype:
        rejected["ckb_return_without_approval"] = reject_ckb(return_ckb("no-return-approval", approve=False), 46)
    if revisable:
        rejected["ckb_return_stale_state"] = reject_ckb(return_ckb("return-stale-state",
            message=packet("return-stale-state", False, script_hash(buyer), 0, content=data)), gate_invalid)
    if state_limit:
        rejected["ckb_return_without_rule"] = reject_ckb(return_ckb("return-missing-rule", deps=[
            dep for dep in ckb_deps if dep != dependency(l1_rule_replacement)]), 46)
    burned = ckb_submit(return_ckb("burn"))
    live_mailbox, authority_point = [point(burned["hash"], i) for i in (0, 2)]
    authority_funds -= FEE
    next_nonce = 1
    rejected["ckb_replay_after_burn"] = reject_ckb(mint_ckb("repeat-after-burn"), 85)
    assert l1.call("get_live_cell", wrapped_point, True)["status"] != "live"

    l1.mine(spec["bridge"]["confirmations"])
    anchor = burned["proof"]["header"]
    advanced = attest_anchor("anchor", anchor)
    mailbox_votes = attest_references("return-mailbox", anchor, (live_mailbox, False))
    certified = mailbox_votes
    if live_rule:
        certified = attest_references("return", anchor, (l1_rule_replacement, True), (live_mailbox, False))
        old = args.output / "object-consumed-rule-request.json"
        write_json(old, l1_reference_request(int(anchor["number"], 16), anchor["hash"], (l1_rule_point, True)))
        refused = subprocess.run([args.binary, "bridge", "attest-reference", "--genesis", spec_path,
            "--l1-rpc", l1.url, "--key-file", fixture.guardian_keys[0], "--request", old,
            "--journal", args.output / "object-consumed-rule"], capture_output=True, text=True)
        assert refused.returncode and "no longer live" in refused.stderr, refused
        rejected["stem_consumed_rule_observation"] = refused.stderr

    release_inputs = [(bond, endpoint, None), (original, endpoint, None)]
    release = [cell(bond_cap, endpoint), cell(object_cap + return_topup, buyer, return_state, types)]
    with_approval = release_inputs + [(native_authority, authority, authority_key)]
    rejected["stem_return_without_policy_approval"] = reject_stem(native_sign("return-no-consent",
        release_inputs + [(native_fee, owner, owner_key)], release + [cell(remainder - FEE - return_topup, owner)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified), "exited with 115")
    rejected["stem_return_policy_removed"] = reject_stem(native_sign("return-without-policy",
        with_approval, [release[0], cell(object_cap + return_topup, buyer, return_state, [identity]),
                        cell(native_auth_capacity - 2 * FEE - return_topup, authority)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified),
        "exited with 54" if metatype else "exited with 117")
    if revisable:
        rejected["stem_return_stale_state"] = reject_stem(native_sign("return-stale-state",
            with_approval, [release[0], cell(object_cap, buyer, data, types),
                            cell(native_auth_capacity - 2 * FEE, authority)],
            endpoint=endpoint, message=inbound, receive=True, votes=certified), "exited with 54")
    if state_limit:
        without_rule = dict(votes=mailbox_votes) if live_rule else dict(votes=certified, **absent)
        rejected["stem_return_under_missing_rule"] = reject_stem(native_sign("return-missing-rule", with_approval,
            release + [cell(native_auth_capacity - 2 * FEE - return_topup, authority)],
            endpoint=endpoint, message=inbound, receive=True, **without_rule), "exited with 56")
    if live_rule:
        rejected["stem_return_under_superseded_rule_anchor"] = reject_stem(native_sign("return-old-rule-anchor", with_approval,
            release + [cell(native_auth_capacity - 2 * FEE, authority)],
            endpoint=endpoint, message=inbound, receive=True, votes=initial_votes),
            "committed CKB anchor")
    if topup:
        rejected["stem_return_without_required_topup"] = reject_stem(native_sign("return-underfunded",
            with_approval, [release[0], cell(object_cap, buyer, return_state, types),
                            cell(native_auth_capacity - 2 * FEE, authority)],
            endpoint=endpoint, message=inbound, receive=True, votes=certified), "insufficient occupied Capacity")
    elif revisable:
        rejected["stem_return_drains_asset_capacity"] = reject_stem(native_sign("return-reduces-capacity",
            with_approval, [release[0], cell(object_cap - 1, buyer, return_state, types),
                            cell(native_auth_capacity - 2 * FEE + 1, authority)],
            endpoint=endpoint, message=inbound, receive=True, votes=certified), "exited with 54")
    redeemed = commit(native_sign("redeem", with_approval,
        release + [cell(native_auth_capacity - 2 * FEE - return_topup, authority)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified))
    restored = signing_cell(client, at(redeemed, 1))
    assert restored["cell"] == release[1]
    spent_escrow = certified_cell(original)
    restored_proof = certified_cell(at(redeemed, 1))
    assert spent_escrow["cell"] is None and spent_escrow["proof"]
    reject_query = native_sign("repeat-release",
        [(at(redeemed, 0), endpoint, None), (at(redeemed, 1), buyer, buyer_key),
         (at(redeemed, 2), authority, authority_key)], release + [cell(native_auth_capacity - 3 * FEE - return_topup, authority)],
        endpoint=endpoint, message=inbound, receive=True, votes=certified)
    rejected["stem_replay_release"] = reject_stem(reject_query, "nonce already used")
    if append_only:
        rewritten = object_data(b"X" + content[1:], 5)
        rejected["stem_rewrite_after_return"] = reject_stem(native_sign("rewrite-after-return",
            [(at(redeemed, 1), buyer, buyer_key), (at(redeemed, 2), authority, authority_key)],
            [cell(object_cap + return_topup, buyer, rewritten, types),
             cell(native_auth_capacity - 3 * FEE - return_topup, authority)], votes=certified),
            "exited with 58")

    def execution(committed):
        location = client.locations[committed["hash"]]
        response = client.query("/stem/execution", height=location["height"],
                                data=hex_bytes(le(location["index"], 4)), prove=True)
        return {**json.loads(bytes.fromhex(response["value"])),
                "height": location["height"], "finality": client.finality(location["height"])["proof"]}

    return {"id": hex_bytes(object_id), "manifest": hex_bytes(manifest), "metatype": metatype,
            "shared_manifest": shared_evidence,
            "external_rule": rule_evidence,
            "append_only": ({"type": append_script, "input": hex_bytes(data),
                             "rejected_outputs": {name: hex_bytes(raw) for name, raw in append_probes.items()}}
                            if append_only else None),
            "revisions": ({**revisions, "exported": hex_bytes(data), "returned": hex_bytes(return_state),
                           "topup_capacity": return_topup,
                           "native_execution": execution(revisions["native"])} if revisable else None),
            "ckb_occupied_shannons": ckb_occupied(fixture.owner_lock, wrapper, projected(data)),
            "identity_type": identity, "policy_type": policy, "wrapper_type": wrapper,
            "escrow": endpoint, "mailbox": mailbox, "mailbox_capacity": mailbox_cap,
            "issued": issued, "exported": exported,
            "ckb_minted": ckb_minted, "ckb_transferred": transferred, "ckb_burned": burned,
            "anchor": advanced, "redeemed": redeemed, "restored": restored,
            "escrowed": escrowed, "spent_escrow": spent_escrow, "restored_proof": restored_proof,
            "outbox": outbox, "inbox": query("/stem/intercell/inbox", inbound["id"])[1],
            "execution": {name: execution(tx) for name, tx in (("issued", issued), ("exported", exported), ("redeemed", redeemed))},
            "rejections": rejected}

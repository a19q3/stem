"""Native Polytype control for the same standalone object transitions."""

import json
import subprocess
import time

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, packed_script, point, script, script_hash
from jolteon_bridge_localnet import Client
from jolteon_localnet import evidence_keys, write_configuration
from localnet import run, write_json
from native_bridge_localnet import cell, signing_cell, start, wait_for


def exercise_polytype(args, fixture, contents, ckb):
    keys = evidence_keys(args)
    ports = [args.port + i for i in range(args.validators)]
    configuration = write_configuration(args.output, [pubkey for pubkey, _ in keys], ports, 150, 1500)
    network = hex_bytes(ckb_hash(b"Stem Polytype object control\0"))
    programs = [args.native_lock.read_bytes(), args.native_object.read_bytes(), args.native_policy.read_bytes()]
    lock_code, object_code, policy_code = [hex_bytes(ckb_hash(code)) for code in programs]
    owner_key, buyer_key, *approval_keys = [fixture.owner, fixture.friend, *fixture.l1_keys[:2]]

    def lock(key):
        return script(lock_code, fixture.key(key)["blake160"], "data")

    owner, buyer, *authorities = [lock(key) for key in (owner_key, buyer_key, *approval_keys)]
    dep_points = [point(hex_bytes(bytes([1]) * 32), index) for index in range(len(programs))]
    deps = [dependency(item) for item in dep_points]
    deposit_points = [point(hex_bytes(bytes([2]) * 32), index) for index in range(3)]
    seed, *approvals = deposit_points
    seed_capacity, approval_capacity, object_capacity = (10_000 * CKB, 200 * CKB, 500 * CKB)
    genesis = {
        "chain_id": "stem-metatype-object-control", "network_id": network,
        "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 1024, "transaction_bytes": 2 * 1024 * 1024,
                   "transaction_cycles": 20_000_000, "block_cycles": 2_000_000_000,
                   "cell_reads": 8192, "cell_read_bytes": 128 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "development_validators": [{"ed25519": hex_bytes(public), "power": i + 1}
                                   for i, (public, _) in enumerate(keys)],
        "development_cells": [
            {"out_point": dep, "cell": cell((len(program) + 200) * CKB, owner, program)}
            for dep, program in zip(dep_points, programs, strict=True)
        ] + [
            {"out_point": dep, "cell": cell(amount, guard)}
            for dep, amount, guard in zip(deposit_points,
                [seed_capacity, approval_capacity, approval_capacity], [owner, *authorities], strict=True)
        ],
    }
    spec = args.output / "polytype-genesis.json"
    write_json(spec, genesis)

    def types(i, index):
        packed_seed = binary(seed["tx_hash"]) + le(int(seed["index"], 16), 4)
        obj = ckb_hash(b"Stem object identity\0" + binary(network) + packed_seed + le(index, 4))
        authority = script_hash(authorities[i])
        identity = script(object_code, hex_bytes(obj + authority + authority + binary(policy_code)), "data")
        policy = script(policy_code, hex_bytes(script_hash(identity) + authority), "data")
        return [identity, policy]

    required = [types(i, i) for i in range(len(contents))]
    clients = [Client(args, port, network) for port in ports]
    client = clients[-1]
    nodes = []
    accepted, rejected = {}, {}

    def signed(name, spending, outputs):
        lock_files = sorted({packed_script(guard): (guard, key) for _, guard, key in spending}.values(),
                            key=lambda row: packed_script(row[0]))
        points = [outpoint for outpoint, _, _ in spending]
        request, draft, context = [args.output / f"polytype-{name}-{suffix}.json"
                                   for suffix in ("request", "draft", "context")]
        write_json(request, {"inputs": [{"out_point": item} for item in points],
                             "outputs": outputs, "cell_deps": deps})
        write_json(context, {"cells": [signing_cell(client, p) for p in points + dep_points]})
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request)))
        for index, (guard, key) in enumerate(lock_files):
            lock_file = args.output / f"polytype-{name}-lock-{index}.json"
            signed_file = args.output / f"polytype-{name}-group-{index}.json"
            write_json(lock_file, guard)
            write_json(signed_file, json.loads(run(args.binary, "wallet", "sign", "--genesis", spec,
                "--transaction", draft, "--context", context, "--group-index", str(index),
                "--lock", lock_file, "--key-file", key)))
            draft = signed_file
        raw = json.loads(draft.read_text())
        write_json(args.output / f"polytype-{name}-signed.json", raw)
        return raw

    def execute(name, spending, outputs):
        tx = signed(name, spending, outputs)
        started = time.monotonic()
        result = client.call("broadcast_tx_commit", tx=tx)
        if result["check_tx"]["code"] != 0:
            raise RuntimeError(result)
        height = result["height"]
        wait_for(lambda: all(peer.status()["height"] >= height for peer in clients),
                 "four-replica finalized object", seconds=25)
        elapsed = (time.monotonic() - started) * 1000
        header = client.header(height)
        assert all(peer.header(height) == header for peer in clients)
        cert = client.finality(height)
        proof_file = args.output / f"polytype-{name}-finality.json"
        write_json(proof_file, cert["proof"])
        verified = json.loads(run(args.binary, "jolteon-proof", "--genesis", spec, "--proof", proof_file))
        assert verified == header
        where = client.locations[result["hash"]]
        proof = client.query("/stem/execution", height=height, data=hex_bytes(le(where["index"], 4)), prove=True)
        execution = json.loads(bytes.fromhex(proof["value"]))
        result_points = [point("0x" + result["hash"], i) for i in range(len(outputs))]
        for outpoint, raw in zip(result_points, outputs, strict=True):
            assert signing_cell(client, outpoint)["cell"] == raw
        accepted[name] = {"transaction": result, "bytes": len(binary(tx)), "execution": execution,
                          "header": header, "finality": cert, "submit_to_all_replicas_ms": elapsed}
        return result_points

    def deny(name, spending, outputs, *expected):
        response = client.call("broadcast_tx_sync", tx=signed(name, spending, outputs))
        if response["code"] != 1 or not any(reason in response["log"] for reason in expected):
            raise RuntimeError(f"{name} mismatch: {response}")
        rejected[name] = response["log"]

    def native_objects(recipient, bodies=contents, script_sets=required):
        return [cell(object_capacity, recipient, body, scripts)
                for body, scripts in zip(bodies, script_sets, strict=True)]

    try:
        for i, (_, key) in enumerate(keys):
            nodes.append(start(args, [args.binary, "run", "--initialize", "--home", args.output / f"polytype-node{i}",
                 "--configuration", configuration, "--genesis", spec, "--key-file", key, "--archive"],
                 f"polytype-node{i}.log"))
        wait_for(lambda: all(peer.status()["height"] >= 3 for peer in clients), "Polytype control startup", seconds=30)

        issuance = [(seed, owner, owner_key), *zip(approvals, authorities, approval_keys, strict=True)]
        fee = seed_capacity - 2 * object_capacity - FEE
        approval_transitions = [cell(approval_capacity, authority) for authority in authorities]
        mint = native_objects(owner) + [*approval_transitions, cell(fee, owner)]
        deny("forged-object-id", issuance,
            native_objects(owner, script_sets=[types(0, 7), required[1]]) + mint[2:], "exited with 117")
        current = execute("issue", issuance, mint)
        spend = [*zip(current[:2], [owner] * 2, [owner_key] * 2, strict=True),
                 *zip(current[2:4], authorities, approval_keys, strict=True), (current[4], owner, owner_key)]
        fee -= FEE
        next_outputs = native_objects(buyer) + [*approval_transitions, cell(fee, buyer)]
        deny("missing-approval", [*spend[:2], spend[4]], next_outputs[:2] + next_outputs[4:], "exited with 115")
        edited = contents[0][:-1] + bytes([contents[0][-1] ^ 1])
        deny("content-edit-with-authority", spend,
            native_objects(buyer, [edited, contents[1]]) + next_outputs[2:], "exited with 117")
        deny("removed-approval-type", spend,
            native_objects(buyer, script_sets=[required[0][:1], required[1]]) + next_outputs[2:], "exited with 117")
        deny("duplicate-object", spend, [next_outputs[0], *next_outputs[:4],
            cell(fee - object_capacity, buyer)], "exited with 117", "exited with 114")
        current = execute("transfer", spend, next_outputs)
        spend = [*zip(current[:2], [buyer] * 2, [buyer_key] * 2, strict=True),
                 *zip(current[2:4], authorities, approval_keys, strict=True), (current[4], buyer, buyer_key)]
        execute("refresh", spend, [*next_outputs[:4], cell(fee - FEE, buyer)])
    finally:
        for process in nodes:
            process.terminate()
        for process in nodes:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    differences = {case: verdict for verdict, native, other in
        [(True, accepted, ckb["results"]), (False, rejected, ckb["rejections"])]
        for case in native if case not in other}
    if differences:
        raise RuntimeError(f"Metatype and native Polytype different outcomes: {differences}")
    return {"genesis": genesis, "accepted": accepted, "rejected": rejected,
            "matched_cases": sorted([*accepted, *rejected]), "content_hashes": [hex_bytes(ckb_hash(x)) for x in contents]}

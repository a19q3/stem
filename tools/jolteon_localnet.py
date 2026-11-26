#!/usr/bin/env python3
"""Run fixed-epoch weighted Jolteon replicas through payment, timeout, and restart."""

import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import socket
import struct
import subprocess
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import CKB, binary, ckb_hash, dependency, hex_bytes, point, script, write_key
from localnet import process_resources, run, write_json
from native_bridge_localnet import cell, start, wait_for

SUBMIT, STATUS, FINALITY, HEADER, HANDOFF, QUERY = 20, 21, 22, 23, 24, 25

PAYMENT_FEES = {
    "encoded_byte_microshannons": 200_000,
    "cycle_microshannons": 35,
    "cell_read_microshannons": 500_000,
    "cell_read_byte_microshannons": 100,
    "cell_write_byte_microshannons": 16_000,
    "cell_delete_microshannons": 1_000_000,
}


def varint(value):
    result = bytearray()
    while value > 127:
        result.append((value & 127) | 128)
        value >>= 7
    result.append(value)
    return bytes(result)


def rpc(port, method, body=b""):
    frame = b"\x08" + varint(0xffffffff) + b"\x10" + varint(method)
    if body:
        frame += b"\x1a" + varint(len(body)) + body
    with socket.create_connection(("127.0.0.1", port), timeout=4) as conn:
        conn.sendall(struct.pack(">I", len(frame)) + frame)
        with conn.makefile("rb") as response:
            length = struct.unpack(">I", response.read(4))[0]
            return json.loads(response.read(length))


def evidence_keys(args):
    result = []
    for slot in range(args.validators):
        raw = hashlib.sha256(f"stem-jolteon-local-fixture-{slot}".encode()).digest()
        key = Ed25519PrivateKey.from_private_bytes(raw)
        public = key.public_key().public_bytes_raw()
        result.append((public, write_key(args.output, f"validator-{slot}", raw)))
    return sorted(result)


def write_configuration(output, keys, ports, cadence, timeout):
    configuration = output / "replicas.json"
    write_json(configuration, {
        "peers": [{"key": key.hex(), "address": f"127.0.0.1:{port}"}
                  for key, port in zip(keys, ports, strict=True)],
        "proposal_interval_ms": cadence, "view_timeout_ms": timeout,
    })
    return configuration


def genesis_and_payments(args, keys, weights, *, fee_calibration=False):
    basic = args.output / "payments"
    basic.mkdir()
    payer = write_key(args.output, "payment-key", bytes([22]) * 32)
    payer_info = json.loads(run(args.binary, "ckb-wallet", "key", "--key-file", payer))
    program = args.lock.read_bytes()
    lock = script(hex_bytes(ckb_hash(program)), payer_info["blake160"], "data")
    network = hex_bytes(ckb_hash(b"Stem Jolteon devnet fixture" + bytes([args.validators])))
    code = point(hex_bytes(bytes([7]) * 32), 0)
    code_cell = cell((len(program) + 200) * CKB, lock, program)
    inputs = [point(hex_bytes(bytes([8]) * 32), index) for index in range(args.transactions)]
    spending = [cell(100 * CKB, lock)] * len(inputs)
    genesis = {
        "chain_id": "stem-jolteon-devnet", "network_id": network, "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 1024, "transaction_bytes": 2 * 1024 * 1024,
                   "transaction_cycles": 20_000_000, "block_cycles": 2_000_000_000,
                   "cell_reads": 8192, "cell_read_bytes": 128 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "development_validators": [{"ed25519": hex_bytes(pub), "power": weights[i]} for i, (pub, _) in enumerate(keys)],
        "development_cells": [{"out_point": code, "cell": code_cell}]
        + [{"out_point": out, "cell": pay} for out, pay in zip(inputs, spending)],
    }
    if fee_calibration:
        genesis["fees"] = PAYMENT_FEES
    spec = args.output / "genesis.json"
    write_json(spec, genesis)
    owner_lock = args.output / "payment-lock.json"
    write_json(owner_lock, lock)

    def sign(index, fee=None):
        out, previous = inputs[index], spending[index]
        suffix = f"{index}-{fee}" if fee is not None else str(index)
        request, draft, context = [basic / f"{suffix}-{file}.json" for file in ("request", "draft", "context")]
        outputs = [cell(99 * CKB, lock)] if fee is None else [cell(50 * CKB, lock), cell(50 * CKB - fee, lock)]
        write_json(request, {"inputs": [{"out_point": out}], "outputs": outputs, "cell_deps": [dependency(code)]})
        write_json(context, {"cells": [{"out_point": out, "cell": previous}, {"out_point": code, "cell": code_cell}]})
        write_json(draft, json.loads(run(args.binary, "wallet", "build", "--request", request)))
        return json.loads(run(args.binary, "wallet", "sign", "--genesis", spec, "--transaction", draft,
                              "--context", context, "--group-index", "0", "--lock", owner_lock, "--key-file", payer))

    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        payments = list(pool.map(lambda i: sign(i, 140 if fee_calibration else None), range(len(inputs))))
    write_json(args.output / "signed-workload.json", payments)
    if fee_calibration:
        write_json(args.output / "underpriced-signed.json", sign(0, 139))
    return spec, payments


def offline_leaders(weights, count):
    slots = []
    credit = [0] * len(weights)
    for _ in range(sum(weights)):
        for i, power in enumerate(weights):
            credit[i] += power
        member = max(range(len(weights)), key=credit.__getitem__)
        credit[member] -= sum(weights)
        slots.append(member)
    if count == 0:
        return [], slots
    cyclic = slots + slots[:count - 1]
    for position in range(len(slots)):
        offline = sorted(set(cyclic[position:position + count]))
        if len(offline) == count and sum(weights[i] for i in offline) * 3 < sum(weights):
            return offline, slots
    raise ValueError("fixture cannot disable that many consecutive leaders and retain quorum")


def scenario(args):
    keys = evidence_keys(args)
    weights = [1 + i % 2 for i in range(args.validators)]
    spec, payments = genesis_and_payments(args, keys, weights, fee_calibration=args.fee_calibration)
    peers = [args.port + i for i in range(args.validators)]
    configuration = write_configuration(
        args.output, [key for key, _ in keys], peers, args.cadence, args.timeout,
    )
    nodes = {}
    phase = "startup"
    alternate = getattr(args, "alternate_binary", None)
    binaries = [alternate if alternate and index % 2 else args.binary for index in range(len(peers))]

    def launch(index, initialize):
        key = keys[index][1]
        command = [binaries[index], "run", "--home", args.output / f"node{index}",
                   "--configuration", configuration, "--genesis", spec, "--key-file", key]
        if index == len(peers) - 1:
            command.append("--archive")
        elif args.history_window is not None:
            command += ["--history-window", str(args.history_window)]
        if initialize:
            command.append("--initialize")
        nodes[index] = start(args, command, f"node{index}-{'initial' if initialize else 'restart'}.log")

    def states(indices):
        latest = []
        for i in indices:
            code = nodes[i].poll()
            if code is not None:
                raise AssertionError(f"replica {i} exited with code {code} during {phase}")
            latest.append(rpc(peers[i], STATUS))
        return latest

    def observed(indices, height, transactions=0):
        latest = states(indices)
        return latest if all(item["height"] >= height and item["committed_transactions"] >= transactions for item in latest) else None

    everybody = list(range(len(peers)))
    start_metrics = {}
    try:
        for i in everybody:
            launch(i, True)
        initial = wait_for(lambda: observed(everybody, 3), "weighted finality at every replica", seconds=30)
        for i, proc in nodes.items():
            start_metrics[i] = process_resources(proc.pid)
        write_json(args.output / "initial.json", initial)
        write_json(args.output / "public-genesis-header.json",
                   json.loads(run(args.binary, "genesis-hash", "--genesis", spec, "--header")))
        fee_rejection = None
        if args.fee_calibration:
            underpriced = json.loads((args.output / "underpriced-signed.json").read_text())
            fee_rejection = rpc(peers[0], SUBMIT, binary(underpriced))
            assert "max cycles exceeded" in fee_rejection.get("error", ""), fee_rejection
            write_json(args.output / "underpriced-payment.json", {"transaction": underpriced, "result": fee_rejection})

        phase = "fault and payment visibility"
        offline, schedule = offline_leaders(weights, args.offline)
        serving = [i for i in everybody if i not in offline]
        for i in offline:
            nodes[i].kill()
            nodes[i].wait(timeout=10)
        fresh = states(serving)
        before = min(item["height"] for item in fresh)
        before_view = max(item["view"] for item in fresh)
        started = time.perf_counter()
        admitted = 0
        relay_retry = None
        if args.require_relay_retry:
            source = serving[0]

            def blocked_routes():
                state = rpc(peers[source], STATUS)
                slot = state["view"] - 1
                leaders = [schedule[(slot + offset) % len(schedule)] for offset in (0, 1)]
                return (state, leaders) if all(leader in offline for leader in leaders) else None

            state, leaders = wait_for(blocked_routes, "two stopped relay targets", seconds=45)
            reply = rpc(peers[source], SUBMIT, binary(payments[0]))
            after = rpc(peers[source], STATUS)
            assert "admitted" in reply and state["view"] == after["view"], (state, reply, after)
            admitted = 1
            delivered = wait_for(lambda: observed(serving, before, admitted),
                                 "single submission delivered through a relay retry", seconds=35)
            retries = delivered[0]["admission"]["relay_retry_attempts"] - state["admission"]["relay_retry_attempts"]
            assert retries > 0 and delivered[0]["admission"]["client_requests"] == 1, delivered[0]
            relay_retry = {"source": source, "leaders": leaders, "before": state, "after_admission": after,
                           "admission": reply, "retries": retries, "delivery": delivered,
                           "seconds_to_visibility": time.perf_counter() - started}
            write_json(args.output / "relay-retry.json", relay_retry)

        for index, item in enumerate(payments[admitted:], start=admitted):
            target = serving[index % len(serving)]
            reply = rpc(peers[target], SUBMIT, binary(item))
            if "admitted" not in reply:
                raise AssertionError(reply)
            admitted += 1
            if args.offered:
                time.sleep(max(0, started + admitted / args.offered - time.perf_counter()))
        def both_payments_and_timeouts():
            active = observed(serving, before + args.offline_blocks, admitted)
            if not active or not offline:
                return active
            tcs = {view for member in active for view in member["timeout_views"] if view >= before_view}
            return active if any(all(
                view + offset in tcs and schedule[(view + offset - 1) % len(schedule)] in offline
                for offset in range(len(offline))
            ) for view in tcs) else None

        # A leader that already served at startup may only be scheduled again
        # after a full weighted rotation. Allow time for the consecutive TCs too.
        rotation_seconds = len(schedule) * args.cadence / 1000
        fault_deadline = max(75, 30 + 1.5 * rotation_seconds + 8 * args.timeout / 1000)
        active = wait_for(both_payments_and_timeouts, "payment finality with the failed leaders",
                          seconds=fault_deadline)
        elapsed = time.perf_counter() - started
        write_json(args.output / "while-offline.json", active)
        phase = "replica restart"
        restart_started = time.perf_counter()
        for i in offline:
            launch(i, False)
        resumed = wait_for(lambda: observed(everybody, min(v["height"] for v in active) + 2, admitted),
                           "restarted replicas recovered certified payloads", seconds=args.recovery_seconds)
        recovery_seconds = time.perf_counter() - restart_started
        if args.offline_blocks > 192:
            assert offline and all(resumed[i]["replayed_blocks"] > 0 for i in offline), resumed
        agreed_height = min(v["height"] for v in resumed)
        headers = [rpc(port, HEADER, struct.pack("<Q", agreed_height)) for port in peers]
        if any(value != headers[0] for value in headers) or "header" not in headers[0]:
            raise AssertionError("replicas finalized different execution Headers")
        proof = rpc(peers[-1], FINALITY, struct.pack("<Q", agreed_height))
        assert "proof" in proof
        proof_file = args.output / "two-chain.json"
        write_json(proof_file, proof["proof"])
        checked = json.loads(run(args.binary, "jolteon-proof", "--genesis", spec, "--proof", proof_file))
        if checked["height"] < agreed_height or (checked["height"] == agreed_height and checked != headers[0]):
            raise AssertionError("proof and committed Header disagree")
        pruning = []
        if args.check_pruning:
            height = agreed_height - args.history_window
            assert height > 0
            for index in everybody:
                old_header = rpc(peers[index], HEADER, struct.pack("<Q", height))
                old_proof = rpc(peers[index], FINALITY, struct.pack("<Q", height))
                assert ("header" in old_header) == (index == everybody[-1]), old_header
                assert ("proof" in old_proof) == (index == everybody[-1]), old_proof
                pruning.append({"replica": index, "height": height, "header": old_header, "proof": old_proof})
        report = {
            "validators": args.validators, "weights": weights, "offline": offline,
            "minimum_offline_blocks": args.offline_blocks, "restart_to_visibility_seconds": recovery_seconds,
            "payments": admitted, "elapsed_until_active_replica_visibility": elapsed, "active_confirmed_tps": admitted / elapsed,
            "archival_replica": everybody[-1], "ordinary_history_window": args.history_window or 256, "pruning": pruning,
            "replica_binaries": [str(path) for path in binaries],
            "before": initial, "with_fault": active, "resumed": resumed, "agreement": headers[0], "two_chain_proof": proof["proof"], "proof_verification": checked,
            "relay_retry": relay_retry,
            "processes": [process_resources(nodes[i].pid) for i in everybody],
            "processes_before": start_metrics,
        }
        if args.checkpoint_proofs:
            heights = range(agreed_height - args.checkpoint_proofs + 1, agreed_height + 1)
            assert heights.start > 0, "not enough finalized blocks for the requested checkpoint proofs"
            checkpoints = []
            for height in heights:
                signed = rpc(peers[-1], FINALITY, struct.pack("<Q", height))
                path = args.output / f"checkpoint-proof-{height}.json"
                assert not signed["handoff"] and signed["epoch"] == 0, signed.keys()
                write_json(path, signed["proof"])
                verified = json.loads(run(args.binary, "jolteon-proof", "--genesis", spec, "--proof", path))
                assert verified == rpc(peers[-1], HEADER, struct.pack("<Q", height)), verified
                checkpoints.append({"height": height, "proof_file": path.name, "header": verified})
            report["checkpoint_proofs"] = checkpoints
        if args.fee_calibration:
            query = json.dumps({"path": "/stem/extinguished-fees", "height": agreed_height, "prove": True}).encode()
            fee = rpc(peers[-1], QUERY, query)
            fee_record = json.loads(bytes.fromhex(fee["value"]))
            assert int(fee_record["extinguished_shannons"], 16) == 140 * admitted, fee_record
            report["fee_calibration"] = {"shannons_per_payment": 140, "encoded_bytes": len(binary(payments[0])),
                                         "underpriced": fee_rejection, "at_height": agreed_height, "proof": fee_record}
        write_json(args.output / "jolteon-report.json", report)
        return report
    finally:
        if len(nodes) == len(peers):
            diagnostics = {}
            for i, proc in nodes.items():
                if proc.poll() is not None:
                    diagnostics[i] = {"exit": proc.poll()}
                else:
                    try:
                        diagnostics[i] = rpc(peers[i], STATUS)
                    except (OSError, ValueError) as exc:
                        diagnostics[i] = {"error": str(exc)}
            write_json(args.output / "last-status.json", diagnostics)
        for child in nodes.values():
            if child.poll() is None:
                child.terminate()
        for child in nodes.values():
            try:
                child.wait(timeout=8)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "lock", "output"):
        parser.add_argument("--" + flag, type=lambda text: Path(text).resolve(), required=True)
    parser.add_argument("--alternate-binary", type=lambda text: Path(text).resolve(),
                        help="use a second executable for odd-numbered replicas in a comparison")
    parser.add_argument("--validators", type=int, default=4)
    parser.add_argument("--transactions", type=int, default=32)
    parser.add_argument("--offline", type=int, default=0)
    parser.add_argument("--offline-blocks", type=int, default=4)
    parser.add_argument("--recovery-seconds", type=int, default=45)
    parser.add_argument("--history-window", type=int)
    parser.add_argument("--check-pruning", action="store_true")
    parser.add_argument("--port", type=int, default=22000)
    parser.add_argument("--cadence", type=int, default=200)
    parser.add_argument("--timeout", type=int, default=850)
    parser.add_argument("--offered", type=float, default=16)
    parser.add_argument("--checkpoint-proofs", type=int, choices=range(0, 5), default=0,
                        help="save verified consecutive exact-height proofs from the live archive")
    parser.add_argument("--fee-calibration", action="store_true")
    parser.add_argument("--require-relay-retry", action="store_true",
                        help="submit one payment when both of its initial relay targets are offline")
    args = parser.parse_args()
    if not 1 <= args.validators <= 128 or min(args.transactions, args.offline_blocks, args.recovery_seconds) < 1:
        parser.error("invalid fixture size")
    if args.check_pruning and (args.history_window is None or args.offline_blocks <= args.history_window):
        parser.error("pruning requires an explicit history window shorter than the offline gap")
    if args.require_relay_retry and args.offline < 2:
        parser.error("relay retry requires at least two offline leaders")
    args.output.mkdir(mode=0o700, parents=True)
    result = scenario(args)
    print(f"{result['validators']} validators finalized {result['payments']} payments "
          f"with {len(result['offline'])} offline, all agreed at height {result['agreement']['height']}")


if __name__ == "__main__":
    main()

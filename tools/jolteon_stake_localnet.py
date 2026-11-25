#!/usr/bin/env python3
"""Use real CKB Stake Cells to change the Jolteon committee and restart a lagging observer."""

import argparse
import json
from pathlib import Path
import struct
import subprocess

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (CKB, FEE, Fixtures, L1, binary, ckb_hash, hex_bytes, le,
                             output, point, script, script_hash, transaction, wallet_group, write_key)
from jolteon_localnet import FINALITY, HANDOFF, HEADER, STATUS, SUBMIT, rpc, write_configuration
from localnet import run, write_json
from native_bridge_localnet import start, wait_for
from stake_localnet import CHAIN, EMPTY, Scenario, mk_record, proof, signable, witness


def public(key):
    return key.public_key().public_bytes_raw()


def publish_checkpoints(args, l1, scenario, fixture, archive, genesis_committee, target_height):
    anchor = scenario.checkpoint
    directory = args.output / "jolteon-checkpoints"
    directory.mkdir()
    initial = le(0, 8) + le(1, 8) + b"".join(binary(member["ed25519"]) + le(member["power"], 8)
                                               for member in genesis_committee)
    genesis = binary(json.loads((args.output / "public-genesis-header.json").read_text()))
    genesis_id = ckb_hash(b"Stem Header\0" + scenario.network + genesis)
    policy = (anchor["id"] + scenario.network + scenario.genesis + genesis_id
              + ckb_hash(b"Stem Jolteon checkpoint roster\0" + initial))
    type_script = script(anchor["hash"], hex_bytes(policy), "data2")
    lock = dict(type_script, args=hex_bytes(script_hash(type_script)))
    roster = directory / "initial-roster.json"
    write_json(roster, hex_bytes(initial))
    previous = None
    publications, rejections = [], {}
    checkpoint = None
    funding = anchor["seed"]

    for height, label in ((10, "handoff-0"), (20, "handoff-1"), (target_height, "post-handoff")):
        command = [args.binary, "checkpoint", "jolteon-bundle", "--archive-db", archive,
                   "--index", directory / "history", "--network", hex_bytes(scenario.network),
                   "--height", str(height), "--policy", hex_bytes(policy), "--roster", roster,
                   "--proof", args.output / f"{label}.json"]
        if label.startswith("handoff-"):
            command.append("--terminal")
        if previous is not None:
            command += ["--previous-tip", previous]
        bundle = json.loads(run(*command))
        write_json(directory / f"{label}-bundle.json", bundle)
        raw = binary(bundle["witness"])

        def sign(name, source, fees, tip, evidence):
            inputs = [source, fees] if source else [fees]
            capacity = 1000 * CKB
            payer = fixture.friend if source else fixture.faucet
            available = int(l1.call("get_live_cell", fees, True)["cell"]["output"]["capacity"], 16)
            change = available - FEE - (capacity if source is None else 0)
            tx = transaction(inputs, [output(capacity, lock, type_script), output(change, fixture.friend_lock)],
                             [tip, "0x"], [anchor["code"], scenario.secp])
            tx["witnesses"] = [witness(evidence)]
            return fixture.sign(name, tx, [wallet_group(len(inputs) - 1, [payer])])

        if checkpoint is None:
            corrupt = bytearray(binary(bundle["tip"]))
            corrupt[-1] ^= 1
            rejections["different_execution_root"] = l1.reject(sign("bad-tip", None, funding, hex_bytes(corrupt), raw))
        if height == target_height:
            # Replay the earlier authenticated handoff against its own live successor.
            old_bundle = json.loads((directory / "handoff-1-bundle.json").read_text())
            rejections["replayed_handoff"] = l1.reject(sign("replay-handoff", checkpoint, funding,
                old_bundle["tip"], binary(old_bundle["witness"])))
        signed = sign(f"ckb-checkpoint-{height}", checkpoint, funding, bundle["tip"], raw)
        estimated = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = l1.send(signed)
        checkpoint, funding = point(tx, 0), point(tx, 1)
        publications.append({"height": height, "transaction": tx, "estimated_cycles": estimated,
                             "witness_bytes": len(raw), "l1_inclusion": l1.prove_committed(tx)})
        previous = directory / f"{label}-tip.json"
        write_json(previous, bundle["tip"])
        roster = directory / f"{label}-next-roster.json"
        write_json(roster, bundle["next_roster"])
    return {"checkpoint_type": hex_bytes(script_hash(type_script)), "publications": publications,
            "rejections": rejections}


def run_epochs(args, l1, scenario, fixture):
    old, incoming, observer = [Ed25519PrivateKey.from_private_bytes(bytes([seed]) * 32)
                               for seed in (31, 32, 33)]
    low, high = 250 * CKB, 400 * CKB
    owner = script_hash(fixture.standard)

    def activate(key, amount, epoch):
        pub = public(key)

        def message(domain, previous, next_):
            return signable(domain, scenario.network, scenario.genesis, scenario.registry_id, previous, next_)

        registered = scenario.change(f"register-{epoch}", 0, pub,
            mk_record(pub, amount, epoch, None, owner + bytes(48)),
            lambda before, after: key.sign(message(b"Stem CKB Stake registration\0", before, after)))
        meta = owner + le(1 + epoch * 10, 8) + le(0, 8) + EMPTY
        activated = scenario.change(f"activate-{epoch}", 3, pub, mk_record(pub, amount, epoch, None, meta),
            lambda before, after: proof(scenario.authority, message(b"Stem CKB Stake activation\0", before, after)))
        return registered, activated, meta

    registered_old, activated_old, old_meta = activate(old, low, 0)
    genesis = scenario.observe("initial", 0, count=3)
    verified, _ = scenario.verify_genesis_snapshot(genesis)
    genesis_committee = verified["validators"]
    assert genesis_committee == [{"ed25519": hex_bytes(public(old)), "power": 250}]
    registered_new, activated_new, new_meta = activate(incoming, high, 1)

    spec = args.output / "stem-genesis.json"
    write_json(spec, {
        "chain_id": CHAIN.decode(), "network_id": hex_bytes(scenario.network), "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024, "transaction_cycles": 20_000_000,
                   "block_cycles": 400_000_000, "cell_reads": 1024, "cell_read_bytes": 64 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "stake": {"trust": "snapshot-trust.json", "observations": "genesis-observations.json", "signing_window": 5,
                  "limits": {"operations": 1, "bytes": 256 * 1024, "signatures": 8}},
    })
    write_json(args.output / "genesis-committee.json", genesis_committee)
    write_json(args.output / "public-genesis-header.json",
               json.loads(run(args.binary, "genesis-hash", "--genesis", spec, "--header")))
    keys = sorted([old, incoming, observer], key=public)
    ports = {public(key): args.port + i for i, key in enumerate(keys)}
    configuration = write_configuration(args.output, [public(key) for key in keys], ports.values(), 750, 2800)
    key_files = {public(key): write_key(args.output, f"jolteon-{i}", key.private_bytes_raw())
                 for i, key in enumerate(keys)}
    processes = {}

    def launch(key, initialize):
        pub = public(key)
        home = args.output / f"node-{ports[pub]}"
        command = [args.binary, "jolteon", "--home", home, "--genesis", spec,
                   "--configuration", configuration, "--key-file", key_files[pub]]
        phase = "initial" if initialize else "restart"
        if initialize:
            command.append("--initialize")
        processes[pub] = start(args, command, f"stem-{ports[pub]}-{phase}.log")

    def request(key, kind, body=b""):
        pub = public(key)
        code = processes[pub].poll()
        if code is not None:
            raise AssertionError(f"node {ports[pub]} exited with code {code}")
        return rpc(ports[pub], kind, body)

    def progress(key, height, epoch):
        def reached():
            reply = request(key, STATUS)
            return reply if reply["height"] >= height and reply["epoch"] == epoch else None

        return wait_for(reached, f"node {ports[public(key)]} epoch {epoch} height {height}", seconds=65)

    def header(key, height):
        return request(key, HEADER, struct.pack("<Q", height))

    def snapshot(epoch, submitter, parent):
        signing_height = 10 * epoch - 5
        progress(submitter, signing_height, epoch - 1)
        anchor = hex_bytes(ckb_hash(b"Stem Header\0" + scenario.network + bytes.fromhex(header(submitter, signing_height)["header"])))
        votes = scenario.observe(f"epoch-{epoch}", epoch, parent, count=3, signing_anchor=anchor)
        attestations = args.output / f"epoch-{epoch}-votes.json"
        write_json(attestations, votes)
        tx = binary(json.loads(run(args.binary, "stake", "snapshot", "--observations", attestations)))
        reply = request(submitter, SUBMIT, tx)
        assert "admitted" in reply, reply
        write_json(args.output / f"epoch-{epoch}-transaction.json", hex_bytes(tx))
        return votes[0]["registry"]

    def verify(raw, label, prior=(), terminal=False):
        path = args.output / f"{label}.json"
        write_json(path, raw)
        command = [args.binary, "jolteon-proof", "--genesis", spec, "--proof", path]
        for epoch in prior:
            command += ["--handoff", args.output / f"handoff-{epoch}.json"]
        if terminal:
            command.append("--terminal")
        checked = json.loads(run(*command))
        return checked

    try:
        for key in keys:
            launch(key, True)
        initial = [progress(key, 3, 0) for key in (old, incoming, observer)]
        assert initial[0]["member"] == 0 and all(status["member"] is None for status in initial[1:])
        assert [status["voting_power"] for status in initial] == [250, None, None]
        if args.replace:
            retired = scenario.change("request-live-exit", 2, public(old), mk_record(public(old), low, 0, 1, old_meta))
        original = snapshot(1, old, genesis[0]["registry"])
        lag_start = progress(observer, 6, 0)
        processes[public(observer)].kill()
        processes[public(observer)].wait(timeout=8)
        joined = [progress(key, 11, 1) for key in (old, incoming)]
        increased = None
        if args.replace:
            assert [status["member"] for status in joined] == [None, 0]
            assert [status["voting_power"] for status in joined] == [None, 400]
            increased = scenario.change("increase-incoming", 1, public(incoming),
                mk_record(public(incoming), 450 * CKB, 1, None, new_meta))
        else:
            assert sorted(status["member"] for status in joined) == [0, 1]
            assert [status["voting_power"] for status in joined] == [250, 400]
            retired = scenario.change("request-live-exit", 2, public(old), mk_record(public(old), low, 0, 2, old_meta))
        snapshot(2, incoming, original)
        current = [progress(key, 21, 2) for key in (old, incoming)]
        assert current[0]["member"] is None and current[1]["member"] == 0
        assert [status["voting_power"] for status in current] == [None, 450 if args.replace else 400]

        # The old process is now outside the certified roster. Restart it on the handed-off journal.
        processes[public(old)].kill()
        processes[public(old)].wait(timeout=8)
        while_old_offline = progress(incoming, 24, 2)
        launch(old, False)
        launch(observer, False)
        caught_up = [progress(key, 25, 2) for key in (old, incoming, observer)]
        common_height = min(status["height"] for status in caught_up)
        agreed = header(incoming, common_height)
        assert all(header(key, common_height) == agreed for key in keys)

        handoffs = []
        for outgoing in (0, 1):
            raw = request(observer, HANDOFF, struct.pack("<Q", outgoing))["proof"]
            authenticated = verify(raw, f"handoff-{outgoing}", range(outgoing), terminal=True)
            assert authenticated == header(incoming, (outgoing + 1) * 10)
            handoffs.append(authenticated)
        ordinary = request(observer, FINALITY, struct.pack("<Q", common_height))
        assert ordinary["epoch"] == 2 and not ordinary["handoff"]
        verified = verify(ordinary["proof"], "post-handoff", (0, 1))
        assert verified["height"] >= common_height and verified == header(incoming, verified["height"])
        wrong_epoch = subprocess.run([args.binary, "jolteon-proof", "--genesis", spec,
            "--proof", args.output / "post-handoff.json", "--handoff", args.output / "handoff-0.json"],
            text=True, capture_output=True)
        assert wrong_epoch.returncode != 0
        tampered = bytearray.fromhex(json.loads((args.output / "handoff-0.json").read_text()))
        tampered[-1] ^= 1
        corrupted_path = args.output / "invalid-handoff.json"
        write_json(corrupted_path, tampered.hex())
        bad_handoff = subprocess.run([args.binary, "jolteon-proof", "--genesis", spec,
            "--proof", corrupted_path, "--terminal"], text=True, capture_output=True)
        assert bad_handoff.returncode != 0
        checkpoints = publish_checkpoints(args, l1, scenario, fixture, args.output / f"node-{ports[public(incoming)]}" / "db",
                                          genesis_committee, verified["height"]) if args.publish_checkpoint else None
        return {
            "replace": args.replace, "l1_transactions": {"registered_old": registered_old, "activated_old": activated_old,
                                "registered_incoming": registered_new, "activated_incoming": activated_new,
                                "exit_requested": retired, "increased_incoming": increased},
            "initial": initial, "observer_stopped": lag_start, "joined": joined, "committee_two": current,
            "after_retired_offline": while_old_offline, "caught_up": caught_up, "agreed": agreed,
            "handoffs": handoffs, "post_handoff_header": verified, "missing_handoff_rejected": wrong_epoch.stderr,
            "altered_handoff_rejected": bad_handoff.stderr,
            "ckb_checkpoints": checkpoints,
        }
    finally:
        for child in processes.values():
            if child.poll() is None:
                child.terminate()
        for child in processes.values():
            try:
                child.wait(timeout=8)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("binary", "l1", "stake-registry", "direct-checkpoint", "output"):
        parser.add_argument(f"--{arg}", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--port", type=int, default=22100)
    parser.add_argument("--l1-port", type=int, default=22110)
    parser.add_argument("--replace", action="store_true", help="replace the entire first committee, then increase power")
    parser.add_argument("--publish-checkpoint", action="store_true", help="publish native Jolteon checkpoints on the CKB devnet")
    args = parser.parse_args()
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    try:
        fixture = Fixtures(args)
        report = run_epochs(args, l1, Scenario(args, l1, fixture), fixture)
        write_json(args.output / "jolteon-stake-report.json", report)
        print(f"Verified two CKB-backed Jolteon handoffs, with all replicas at height {report['agreed']['height']}")
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

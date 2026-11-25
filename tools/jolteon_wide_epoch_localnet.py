#!/usr/bin/env python3
"""Rotate a full funded CKB Stake Registry committee and verify its boundary on CKB."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (
    CKB, FEE, Fixtures, L1, binary, ckb_hash, hex_bytes, le, output, point,
    transaction, wallet_group, write_key,
)
from intercell_epoch_localnet import EpochMailbox
from jolteon_bridge_localnet import Client
from jolteon_checkpoint_localnet import Relay
from jolteon_localnet import FINALITY, HANDOFF, HEADER, STATUS, SUBMIT, rpc, write_configuration
from localnet import process_resources, run, write_json
from native_bridge_localnet import bridge_configuration, deploy, deposit_and_mint, start, wait_for
from stake_localnet import CHAIN, Scenario, mk_record, witness


def public(key):
    return key.public_key().public_bytes_raw()


def make_committee(args, stake):
    keys = [
        Ed25519PrivateKey.from_private_bytes(hashlib.sha256(f"funded-wide-stem-{i}".encode()).digest())
        for i in range(args.validators + 1)
    ]
    incoming = keys.pop()
    keys.sort(key=public)
    power = {public(key): 150 + (i % 4) * 15 for i, key in enumerate(keys)}
    metadata, history = {}, []
    started = time.perf_counter()
    for index, key in enumerate(keys):
        pub = public(key)
        registered, activated, metadata[pub] = stake.activate_validator(f"genesis-{index}", key, power[pub] * CKB, 0)
        history.append({"validator": hex_bytes(pub), "power": power[pub], "register": registered, "activate": activated,
                        "registry_sequence": stake.sequence, "elapsed_seconds": time.perf_counter() - started})
        write_json(args.output / "funded-stake-history.json", history)
    genesis = stake.observe("genesis", 0, count=3)
    checked, quorum_rejection = stake.verify_genesis_snapshot(genesis,
        max_validators=args.validators, max_total_power=args.validators * 1000)
    roster = [{"ed25519": hex_bytes(pub), "power": weight} for pub, weight in sorted(power.items())]
    assert checked["validators"] == roster, checked
    retiring, paused, topped = map(public, keys[:3])
    replacement = public(incoming)
    registered, activated, _ = stake.activate_validator("incoming", incoming, 255 * CKB, 1)
    retired = stake.change("retiring-validator", 2, retiring, mk_record(
        retiring, power[retiring] * CKB, 0, 1, metadata[retiring]))
    increased = stake.change("top-up-continuing-validator", 1, topped, mk_record(
        topped, (power[topped] + 40) * CKB, 0, None, metadata[topped]))
    successor = {**power, replacement: 255, topped: power[topped] + 40}
    successor.pop(retiring)
    changes = {"incoming": hex_bytes(replacement), "retiring": hex_bytes(retiring), "paused": hex_bytes(paused),
               "topped_up": hex_bytes(topped), "registered": registered, "activated": activated,
               "requested_exit": retired, "increased_stake": increased, "registry_sequence": stake.sequence,
               "registry": point(stake.registry_tx, 0)}
    write_json(args.output / "committee-changes.json", changes)
    return (sorted(keys + [incoming], key=public), power, successor, roster, genesis[0]["registry"],
            quorum_rejection, changes)


def reject_skipped_handoff(args, l1, relay, stake, boundary, ordinary_height, ordinary_proof):
    # Build a valid successor bundle under the boundary and try it against the old
    # L1 OutPoint. Its proof and state differ from the incumbent's pinned roster.
    correct = relay.bundle(boundary, proof_file=args.output / "handoff-0.json", terminal=True)
    tip = relay.root / "unpublished-boundary-tip.json"
    roster = relay.root / "unpublished-successor-roster.json"
    write_json(tip, correct["tip"])
    write_json(roster, correct["next_roster"])
    previous = relay.previous, relay.roster
    relay.previous, relay.roster = tip, roster
    try:
        wrong = relay.bundle(ordinary_height, proof_file=ordinary_proof)
    finally:
        relay.previous, relay.roster = previous

    funding = int(l1.call("get_live_cell", relay.funding, True)["cell"]["output"]["capacity"], 16)
    tx = transaction([relay.point, relay.funding],
        [output(1000 * CKB, relay.lock, relay.type), output(funding - FEE, relay.fixture.friend_lock)],
        [wrong["tip"], "0x"], [relay.anchor["code"], stake.secp])
    tx["witnesses"] = [witness(binary(wrong["witness"]))]
    signed = relay.fixture.sign("ckb-reject-skipped-wide-handoff", tx, [wallet_group(1, [relay.fixture.friend])])
    rejected = l1.reject(signed)
    assert "error code 43" in rejected.lower(), rejected
    return rejected


def run_epochs(args, l1, stake, fixture):
    keys, initial, successor, roster, parent, insufficient, changes = make_committee(args, stake)
    ports = {public(key): args.port + index for index, key in enumerate(keys)}
    key_files = {public(key): write_key(args.output, f"validator-{index}", key.private_bytes_raw())
                 for index, key in enumerate(keys)}
    configuration = write_configuration(args.output, ports.keys(), ports.values(), args.cadence, args.timeout)
    paused = binary(changes["paused"])
    archive = binary(changes["incoming"])
    anchor_height = args.blocks_per_epoch - args.signing_window
    spec = args.output / "stem-genesis.json"
    network_spec = {
        "chain_id": CHAIN.decode(), "network_id": hex_bytes(stake.network), "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024,
                   "transaction_cycles": 20_000_000, "block_cycles": 400_000_000, "cell_reads": 1024,
                   "cell_read_bytes": 64 * 1024 * 1024, "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "stake": {"trust": "snapshot-trust.json", "observations": "genesis-observations.json",
                  "signing_window": args.signing_window,
                  "limits": {"operations": 1, "bytes": 256 * 1024, "signatures": 8}},
    }
    mailbox = None
    if args.intercell_mailbox:
        network = network_spec["network_id"]
        funding = dict(stake.code["out_point"], index="0x2")
        vault = deploy(args, l1, fixture, network, funding, checkpoint_anchor=stake.checkpoint)
        mailbox = EpochMailbox(args, l1, fixture, vault, spec, network)
        bridge, lane = bridge_configuration(args, l1, fixture, vault, None, network)
        bridge["intercell_mailbox_code"] = mailbox.type["code_hash"]
        network_spec["bridge"] = bridge
    write_json(spec, network_spec)
    write_json(args.output / "genesis-committee.json", roster)
    write_json(args.output / "public-genesis-header.json",
        json.loads(run(args.binary, "genesis-hash", "--genesis", spec, "--header")))
    processes = {}

    def home(pub):
        return args.output / f"node-{ports[pub]}"

    def launch(pub, initialize):
        command = [args.binary, "run", "--home", home(pub), "--genesis", spec,
                   "--configuration", configuration, "--key-file", key_files[pub]]
        if pub == archive:
            command.append("--archive")
        if initialize:
            command.append("--initialize")
        label = "initial" if initialize else "restart"
        processes[pub] = start(args, command, f"node-{ports[pub]}-{label}.log")

    def request(pub, method, payload=b""):
        assert processes[pub].poll() is None, (ports[pub], processes[pub].poll())
        return rpc(ports[pub], method, payload)

    def statuses(pubkeys, height, epoch):
        values = {hex_bytes(pub): request(pub, STATUS) for pub in pubkeys}
        if all(value["height"] >= height and value["epoch"] == epoch for value in values.values()):
            return values

    def verify(path, *, boundary=False, successor_proof=False):
        command = [args.binary, "jolteon-proof", "--genesis", spec, "--proof", path]
        if boundary:
            command.append("--terminal")
        if successor_proof:
            command += ["--handoff", args.output / "handoff-0.json"]
        return json.loads(run(*command))

    duration = {}
    try:
        begin = time.perf_counter()
        for pub in ports:
            launch(pub, True)
        before = wait_for(lambda: statuses(ports, 3, 0), "full initial committee", seconds=90)
        assert {pub: value["voting_power"] for pub, value in before.items()
                if value["member"] is not None} == {hex_bytes(p): w for p, w in initial.items()}, before
        duration["startup_seconds"] = time.perf_counter() - begin
        write_json(args.output / "before-handoff.json", before)
        paused_before_stop = request(paused, STATUS)
        assert paused_before_stop["epoch"] == 0 and paused_before_stop["height"] < anchor_height, paused_before_stop
        write_json(args.output / "paused-before-stop.json", paused_before_stop)
        processes[paused].kill()
        processes[paused].wait(timeout=10)

        live = [pub for pub in ports if pub != paused]
        if mailbox:
            incumbent = next(pub for pub in initial if pub != paused)
            client = Client(args, ports[incumbent], network)
            mint = deposit_and_mint(args, l1, fixture, vault, network_spec, spec, lane, client)
            message, sent = mailbox.send(client, "incumbent", 41, b"from the incumbent committee",
                                         mint["payment"], mint["payment_capacity"], mint["lock_dep"])
            old_proof = mailbox.proof(client, message)
            assert sent["height"] <= old_proof[0] < args.blocks_per_epoch
            write_json(args.output / "epoch-intercell-original-outbox.json", old_proof)
        wait_for(lambda: request(archive, STATUS)["height"] >= anchor_height,
                 "epoch 1 signing anchor", seconds=100)
        signed_anchor = request(archive, HEADER, le(anchor_height, 8))
        anchor = hex_bytes(ckb_hash(b"Stem Header\0" + stake.network + bytes.fromhex(signed_anchor["header"])))
        vote_start = time.perf_counter()
        votes = stake.observe("epoch-1", 1, parent, count=3, signing_anchor=anchor)
        duration["guardian_observation_seconds"] = time.perf_counter() - vote_start
        attestations = args.output / "epoch-1-votes.json"
        write_json(attestations, votes)
        snapshot = json.loads(run(args.binary, "stake", "snapshot", "--observations", attestations))
        write_json(args.output / "epoch-1-transaction.json", snapshot)
        before_submit = request(archive, STATUS)
        outgoing = next(pub for pub in initial if pub != paused and pub != binary(changes["retiring"]))
        admission = request(outgoing, SUBMIT, binary(snapshot))
        assert "admitted" in admission, (before_submit, admission)
        duration["snapshot_admission_height"] = before_submit["height"]
        duration["anchor_to_admission_seconds"] = time.perf_counter() - vote_start
        write_json(args.output / "snapshot-admission.json", {"anchor": signed_anchor, "reply": admission,
                   "before_submit": before_submit, "timing": duration})

        after_height = args.blocks_per_epoch + 2
        boundary_started = time.perf_counter()
        live_status = wait_for(lambda: statuses(live, after_height, 1), "full successor committee", seconds=100)
        duration["admission_to_successor_visible_seconds"] = time.perf_counter() - boundary_started
        write_json(args.output / "after-handoff-before-restart.json", live_status)
        restarted = time.perf_counter()
        launch(paused, False)
        states = wait_for(lambda: statuses(ports, after_height + 2, 1), "offline voter recovering across handoff", seconds=90)
        duration["restart_seconds"] = time.perf_counter() - restarted
        assert {pub: value["voting_power"] for pub, value in states.items()
                if value["member"] is not None} == {hex_bytes(p): w for p, w in successor.items()}, states
        common = min(value["height"] for value in states.values())
        agreed = request(archive, HEADER, le(common, 8))
        assert all(request(pub, HEADER, le(common, 8)) == agreed for pub in ports)
        write_json(args.output / "after-handoff.json", states)
        returned = states[hex_bytes(paused)]
        assert returned["committed_transactions"] > paused_before_stop["committed_transactions"], returned
        write_json(args.output / "replica-resources.json", {hex_bytes(p): process_resources(processes[p].pid) for p in ports})

        handoff = args.output / "handoff-0.json"
        write_json(handoff, request(archive, HANDOFF, le(0, 8))["proof"])
        boundary = verify(handoff, boundary=True)
        assert boundary["height"] == args.blocks_per_epoch
        assert request(archive, HEADER, le(args.blocks_per_epoch, 8)) == boundary
        assert request(paused, HEADER, le(args.blocks_per_epoch, 8)) == boundary
        earlier = args.output / "before-finality.json"
        later = args.output / "successor-finality.json"
        write_json(earlier, request(archive, FINALITY, le(anchor_height, 8))["proof"])
        write_json(later, request(archive, FINALITY, le(after_height, 8))["proof"])
        assert verify(earlier) == signed_anchor
        assert verify(later, successor_proof=True) == request(archive, HEADER, le(after_height, 8))
        wrong = subprocess.run([args.binary, "jolteon-proof", "--genesis", spec, "--proof", later],
                               text=True, capture_output=True, timeout=15)
        assert wrong.returncode and not wrong.stdout, wrong
        missing = wrong.stderr.splitlines()[0]

        client = Client(args, ports[archive], hex_bytes(stake.network))
        relay = Relay(args, l1, fixture, stake.checkpoint, home(archive) / "db", spec, roster, client)
        relay.advance(anchor_height, stake.secp, proof_file=earlier)
        skipped = reject_skipped_handoff(args, l1, relay, stake, args.blocks_per_epoch, after_height, later)
        relay.advance(args.blocks_per_epoch, stake.secp, proof_file=handoff, terminal=True)
        relay.advance(after_height, stake.secp, proof_file=later, probe=True)
        intercell = None
        if mailbox:
            retired = binary(changes["retiring"])
            observer = Client(args, ports[retired], hex_bytes(stake.network))
            observer_before = observer.status()
            assert observer_before["epoch"] == 1 and observer_before["member"] is None, observer_before
            _, successor_send = mailbox.send(observer, "successor", 42, b"after the committee change",
                point("0x" + sent["hash"], 0), mint["payment_capacity"] - FEE, mint["lock_dep"])
            wait_for(lambda: statuses((archive,), successor_send["height"], 1),
                     "retired observer's transaction finalized on archive", seconds=30)
            new_proof = mailbox.proof(client, message)
            assert new_proof[0] >= successor_send["height"] > after_height
            wait_for(lambda: statuses(ports, new_proof[0], 1), "Outbox proof height on all replicas", seconds=30)
            tip = request(archive, HEADER, le(new_proof[0], 8))
            assert all(request(pub, HEADER, le(new_proof[0], 8)) == tip for pub in ports)
            checkpoint = relay.advance(new_proof[0], stake.secp)
            intercell = mailbox.deliver(relay, message, old_proof, new_proof, checkpoint)
            intercell.update({"mint": mint, "source_send": sent, "successor_send": successor_send,
                              "successor_submitter": hex_bytes(retired), "submitter_status": observer_before,
                              "agreed_successor_header": tip})
        native = {
            "validators": args.validators, "processes": len(ports), "initial_committee": roster,
            "successor_committee": [{"ed25519": hex_bytes(p), "power": w} for p, w in sorted(successor.items())],
            "changes": changes, "previous_l1_anchor": parent, "new_l1_anchor": votes[0]["registry"],
            "genesis_quorum_rejection": insufficient, "snapshot_admission": admission, "snapshot_bytes": len(binary(snapshot)),
            "boundary": boundary, "handoff_bytes": len(binary(json.loads(handoff.read_text()))),
            "agreed": agreed, "paused_voter": {"before": paused_before_stop, "returned": returned},
            "missing_handoff_rejected": missing, "skipped_ckb_handoff_rejected": skipped,
            "checkpoint": relay.report(), "durations": duration,
        }
        if intercell:
            native["intercell"] = intercell
        write_json(args.output / "wide-handoff-report.json", native)
        print(f"{args.validators} to {args.validators} funded stake handoff confirmed on CKB at Stem {boundary['height']}")
    finally:
        for process in processes.values():
            if process.poll() is None:
                process.terminate()
        for process in processes.values():
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "stake-registry", "direct-checkpoint", "output"):
        parser.add_argument("--" + name, type=lambda path: Path(path).resolve(), required=True)
    for name in ("intercell-mailbox", "vault", "deposit-lock", "native-lock", "wrapped"):
        parser.add_argument("--" + name, type=lambda path: Path(path).resolve())
    parser.add_argument("--validators", type=int, default=128)
    parser.add_argument("--port", type=int, default=29630)
    parser.add_argument("--l1-port", type=int, default=29780)
    parser.add_argument("--cadence", type=int, default=450)
    parser.add_argument("--timeout", type=int, default=3000)
    parser.add_argument("--blocks-per-epoch", type=int, default=40)
    parser.add_argument("--signing-window", type=int, default=30)
    parser.add_argument("--ckb-block-cycles", type=int, default=3_500_000_000)
    args = parser.parse_args()
    if not 4 <= args.validators <= 128 or not 4 <= args.signing_window <= args.blocks_per_epoch - 4:
        parser.error("invalid full committee or epoch dimensions")
    if args.intercell_mailbox and not all((args.vault, args.deposit_lock, args.native_lock, args.wrapped)):
        parser.error("--intercell-mailbox needs the bridge contract binaries")
    args.xudt = None
    args.output.mkdir(parents=True, mode=0o700)
    capacity = (2000 + (args.validators + 1) * 171) * CKB
    funding = capacity + (args.validators + 1) * 1000 * CKB
    l1 = L1(args)
    try:
        assert int(l1.call("get_consensus")["max_block_cycles"], 16) == args.ckb_block_cycles
        fixture = Fixtures(args)
        stake = Scenario(args, l1, fixture, registry_capacity=capacity, registry_funding=funding,
                         blocks_per_epoch=args.blocks_per_epoch)
        run_epochs(args, l1, stake, fixture)
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Use real CKB Stake Cells to change the Jolteon committee and restart a lagging observer."""

import argparse
import json
from pathlib import Path
import struct
import subprocess
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import CKB, FEE, Fixtures, L1, binary, ckb_hash, hex_bytes, le, script_hash, write_key
from jolteon_bridge_localnet import Client
from jolteon_checkpoint_localnet import Relay
from jolteon_localnet import FINALITY, HANDOFF, HEADER, STATUS, SUBMIT, rpc, write_configuration
from jolteon_slash_localnet import evidence as slash_evidence
from localnet import run, write_json
from native_bridge_localnet import start, wait_for
from stake_localnet import CHAIN, DELAY, EMPTY, Scenario, mk_record, proof, signable


def public(key):
    return key.public_key().public_bytes_raw()


def retirement_marker(client, validator, effective):
    response = client.query("/stem/stake-retirement", data=hex_bytes(validator), prove=True)
    marker = json.loads(bytes.fromhex(response["value"]))
    assert marker["effective_height"] == effective
    return marker, response["height"]


def freeze_fixture(args, scenario, relay, signer, epoch, sample_height, checkpoint_height, old_meta, low,
                   exit_epoch, dependency):
    validator = public(signer)
    raw, forged, late, penalty = slash_evidence(args, relay, signer, epoch, sample_height, checkpoint_height)
    frozen_meta = old_meta[:48] + penalty
    frozen_record = mk_record(validator, low, 0, exit_epoch, frozen_meta, frozen=True)
    failures = {
        "forged_vote": scenario.change("freeze-forged-vote", 6, validator, frozen_record,
            lambda *_: forged, dependencies=[dependency], expected="error code 25"),
        "outside_committee_interval": scenario.change("freeze-late-vote", 6, validator, frozen_record,
            lambda *_: late, dependencies=[dependency], expected="error code 23"),
        "freeze_missing_checkpoint": scenario.change("freeze-no-checkpoint", 6, validator, frozen_record,
            lambda *_: raw, expected="error code 23"),
    }
    tx = scenario.change("freeze-double-vote", 6, validator, frozen_record,
        lambda *_: raw, dependencies=[dependency], measure=True, relayer=True)
    failures["replayed_double_vote"] = scenario.change("freeze-replay", 6, validator, frozen_record,
        lambda *_: raw, dependencies=[dependency], expected="error code 23")
    failures["slash_before_retirement"] = scenario.change("slash-before-retire", 8, validator,
        mk_record(validator, 0, 0, exit_epoch, frozen_meta, frozen=True), fee=low + FEE, expected="error code 23")
    return tx, frozen_meta, failures


def publish_checkpoints(args, l1, scenario, relay, spec, client, target_height, exit_case, active_freeze):
    frozen, slashed = None, None
    retired = None
    failures = {}
    since = (1 << 63) | DELAY
    if exit_case:
        signer, old_meta, low, effective, early_marker = exit_case
        validator = public(signer)
        epoch = None if active_freeze else (effective - 1) // 10
        if active_freeze:
            frozen, old_meta, failures = active_freeze
        old_record = mk_record(validator, 0, 0, epoch, old_meta, True, bool(frozen))
        retired_meta = old_meta[:40] + le(effective, 8) + old_meta[48:]
        retired_record = mk_record(validator, low, 0, epoch, retired_meta, frozen=bool(frozen))
        final_record = mk_record(validator, 0, 0, epoch, retired_meta, True, bool(frozen))

    for height, label in ((10, "handoff-0"), (20, "handoff-1"), (target_height, "post-handoff")):
        if relay.publications and height <= relay.publications[-1]["height"]:
            continue
        dependency = relay.advance(height, scenario.secp, proof_file=args.output / f"{label}.json",
                                   terminal=height != target_height, probe=height in (10, target_height))
        if exit_case and height == effective - 1:
            failures["not_yet_inactive"] = scenario.change("retire-early", 4, validator, retired_record,
                lambda *_: binary(early_marker["proof"]), dependencies=[dependency], expected="error code 23")
            l1.mine(DELAY)
            failures["withdraw_without_retirement"] = scenario.change("unretired-withdrawal", 5, validator, old_record,
                since=since, expected="error code 23")
    if exit_case:
        if args.slash and not frozen:
            sample_epoch = 0 if args.replace else 1
            frozen, frozen_meta, rejected = freeze_fixture(args, scenario, relay, signer, sample_epoch,
                3 if args.replace else 12, target_height, old_meta, low, epoch, dependency)
            failures.update(rejected)
            retired_meta = retired_meta[:48] + frozen_meta[48:]
            retired_record = mk_record(validator, low, 0, epoch, retired_meta, frozen=True)
            final_record = mk_record(validator, 0, 0, epoch, retired_meta, True, True)
        marker, captured = retirement_marker(client, validator, effective)
        # Empty execution after the last announcement preserves the SystemRoot.
        assert relay.header(captured)[112:144] == relay.header(target_height)[112:144]
        marker_file = args.output / "effective-retirement-proof.json"
        header_file = args.output / "effective-retirement-header.json"
        write_json(marker_file, marker)
        write_json(header_file, hex_bytes(relay.header(target_height)))
        witness = binary(json.loads(run(args.binary, "stake", "retirement-witness", "--genesis", spec,
            "--validator", hex_bytes(validator), "--marker", marker_file, "--header", header_file)))
        failures["missing_checkpoint"] = scenario.change("retire-no-checkpoint", 4, validator, retired_record,
            lambda *_: witness, expected="error code 23")
        failures["modified_path"] = scenario.change("retire-bad-path", 4, validator, retired_record,
            lambda *_: witness[:-1] + bytes([witness[-1] ^ 1]), dependencies=[dependency], expected="error code 23")
        retirement = scenario.change("retire-excluded", 4, validator, retired_record, lambda *_: witness,
            dependencies=[dependency], relayer=True, measure=True)
        failures["retirement_replayed"] = scenario.change("retirement-replay", 4, validator, retired_record,
            lambda *_: witness, dependencies=[dependency], expected="error code 23")
        if frozen:
            l1.mine(DELAY)
            failures["withdraw_before_slash"] = scenario.change("unbond-before-slash", 5, validator, final_record,
                since=since, expected="error code 23")
            slash_record = mk_record(validator, 0, 0, epoch, retired_meta, frozen=True)
            failures["unfunded_slash"] = scenario.change("slash-unfunded", 8, validator, slash_record,
                expected="error code 23")
            slashed = scenario.change("finalize-slash", 8, validator, slash_record, fee=low + FEE, measure=True)
        failures["immature"] = scenario.change("early-unbonding", 5, validator, final_record,
            since=since, expected="Immature")
        l1.mine(DELAY)
        failures["owner_required"] = scenario.change("unbond-by-relayer", 5, validator, final_record,
            since=since, expected="error code 23", relayer=True)
        unbonded = scenario.change("unbond-retired", 5, validator, final_record, since=since, measure=True)
        assert validator not in scenario.records and validator in scenario.retired
        retired = {"effective_height": effective, "retirement": retirement, "unbonding": unbonded,
                   "bond_shannons": low, "retirement_l1_proof": l1.prove_committed(retirement),
                   "unbonding_l1_proof": l1.prove_committed(unbonded), "rejections": failures,
                   "measurements": scenario.measurements, "freeze": frozen, "slash": slashed,
                   "released_bond": 0 if slashed else low, "slashed_bond": low if slashed else 0}
        if slashed:
            retired["frozen_before_snapshot"] = bool(active_freeze)
            retired["slash_l1_proof"] = l1.prove_committed(slashed)
            retired["freeze_l1_proof"] = l1.prove_committed(frozen)
    return relay.report(), retired


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
    cadence, timeout = (1400, 3600) if args.active_slash else (750, 2800)
    configuration = write_configuration(args.output, [public(key) for key in keys], ports.values(), cadence, timeout)
    key_files = {public(key): write_key(args.output, f"jolteon-{i}", key.private_bytes_raw())
                 for i, key in enumerate(keys)}
    processes = {}

    def launch(key, initialize, home=None):
        pub = public(key)
        data_dir = home or args.output / f"node-{ports[pub]}"
        command = [args.binary, "run", "--home", data_dir, "--genesis", spec,
                   "--configuration", configuration, "--key-file", key_files[pub]]
        if pub == public(incoming):
            command.append("--archive")
        else:
            command += ["--history-window", "64"]
        phase = "initial" if initialize else "snapshot-restart" if home else "restart"
        if initialize:
            command.append("--initialize")
        processes[pub] = start(args, command, f"stem-{ports[pub]}-{phase}.log")

    def request(key, kind, body=b""):
        pub = public(key)
        code = processes[pub].poll()
        if code is not None:
            raise AssertionError(f"node {ports[pub]} exited with code {code}")
        return rpc(ports[pub], kind, body)

    def progress(key, height, epoch, seconds=65):
        def reached():
            reply = request(key, STATUS)
            return reply if reply["height"] >= height and reply["epoch"] == epoch else None

        return wait_for(reached, f"node {ports[public(key)]} epoch {epoch} height {height}", seconds=seconds)

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
        stake_client = Client(args, ports[public(incoming)], hex_bytes(scenario.network))
        archive = args.output / f"node-{ports[public(incoming)]}" / "db"
        relay = Relay(args, l1, fixture, scenario.checkpoint, archive, spec, genesis_committee, stake_client) if args.publish_checkpoint else None
        active_freeze = None
        early = None
        if args.retire and args.replace:
            early, marker_at = retirement_marker(stake_client, public(old), 11)
            assert bytes.fromhex(header(incoming, marker_at)["header"])[112:144] == bytes.fromhex(header(incoming, 10)["header"])[112:144]
        if args.replace:
            assert [status["member"] for status in joined] == [None, 0]
            assert [status["voting_power"] for status in joined] == [None, 400]
            increased = scenario.change("increase-incoming", 1, public(incoming),
                mk_record(public(incoming), 450 * CKB, 1, None, new_meta))
        else:
            assert sorted(status["member"] for status in joined) == [0, 1]
            assert [status["voting_power"] for status in joined] == [250, 400]
            if args.active_slash:
                handoff_proof = request(incoming, HANDOFF, le(0, 8))["proof"]
                verify(handoff_proof, "handoff-0", terminal=True)
                relay.advance(10, scenario.secp, proof_file=args.output / "handoff-0.json", terminal=True, probe=True)
                progress(incoming, 12, 1)
                active_dep = relay.advance(12, scenario.secp)
                active_freeze = freeze_fixture(args, scenario, relay, old, 1, 12, 12, old_meta, low, None, active_dep)
                retired = None
            else:
                retired = scenario.change("request-live-exit", 2, public(old), mk_record(public(old), low, 0, 2, old_meta))
        snapshot(2, incoming, original)
        current = [progress(key, 21, 2) for key in (old, incoming)]
        assert current[0]["member"] is None and current[1]["member"] == 0
        assert [status["voting_power"] for status in current] == [None, 450 if args.replace else 400]
        if args.retire and not args.replace:
            early, marker_at = retirement_marker(stake_client, public(old), 21)
            assert bytes.fromhex(header(incoming, marker_at)["header"])[112:144] == bytes.fromhex(header(incoming, 20)["header"])[112:144]

        # The old process is now outside the certified roster. Restart it on the handed-off journal.
        processes[public(old)].kill()
        processes[public(old)].wait(timeout=8)
        while_old_offline = progress(incoming, 24 + args.observer_extra_lag, 2,
                                     seconds=65 + args.observer_extra_lag * cadence / 1000)
        write_json(args.output / "before-restart.json", {"observer": lag_start, "incoming": while_old_offline})
        restart_started = time.perf_counter()
        launch(old, False)
        launch(observer, False)
        caught_up = [progress(key, 25 + args.observer_extra_lag, 2) for key in (old, incoming, observer)]
        recovery_seconds = time.perf_counter() - restart_started
        if args.observer_extra_lag > 192:
            assert caught_up[0]["replayed_blocks"] > 0 and caught_up[2]["replayed_blocks"] > 0, caught_up
        common_height = min(status["height"] for status in caught_up)
        agreed = header(incoming, common_height)
        assert all(header(key, common_height) == agreed for key in keys)
        write_json(args.output / "catchup-verified.json", {"replicas": caught_up, "header": agreed,
                                                          "restart_to_visibility_seconds": recovery_seconds})

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
        checkpoints = exit_report = None
        if args.publish_checkpoint:
            exit_case = (old, old_meta, low, 11 if args.replace else 21, early) if args.retire else None
            checkpoints, exit_report = publish_checkpoints(args, l1, scenario, relay, spec,
                stake_client, verified["height"], exit_case, active_freeze)
        restored = None
        if args.snapshot_restore:
            proc = processes[public(observer)]
            proc.terminate()
            proc.wait(timeout=8)
            snapshot = args.output / "observer.snapshot"
            certificate = args.output / "observer-certificate.json"
            export = json.loads(run(args.binary, "jolteon-snapshot", "export",
                "--home", args.output / f"node-{ports[public(observer)]}", "--genesis", spec,
                "--snapshot", snapshot, "--certificate", certificate))
            height = export["height"]
            relay.advance(height, scenario.secp)
            l1.mine(4)
            live = l1.call("get_live_cell", relay.point, True)
            assert live["status"] == "live" and live["cell"]["output"]["type"] == relay.type, live
            raw = binary(live["cell"]["data"]["content"])[64:]
            trusted = hex_bytes(ckb_hash(b"Stem Header\0" + scenario.network + raw))
            assert export["header_id"] == trusted[2:], export
            home = args.output / "restored-observer"
            restore = [args.binary, "jolteon-snapshot", "restore", "--home", home, "--genesis", spec,
                       "--history-window", "64",
                       "--snapshot", snapshot, "--certificate", certificate, "--key-file", key_files[public(observer)]]
            wrong = subprocess.run([*restore, "--trusted-header", "11" * 32], capture_output=True, text=True)
            assert wrong.returncode != 0 and "pinned finalized Header" in wrong.stderr and not home.exists(), wrong
            damaged = bytearray(snapshot.read_bytes())
            damaged[-1] ^= 1
            altered = args.output / "altered-observer.snapshot"
            altered.write_bytes(damaged)
            broken = restore.copy()
            broken[broken.index(snapshot)] = altered
            rejected = subprocess.run([*broken, "--trusted-header", trusted], capture_output=True, text=True)
            assert rejected.returncode != 0 and not home.exists(), rejected
            result = json.loads(run(*restore, "--trusted-header", trusted))
            assert result["observer_only"] and result["height"] == height, result
            launch(observer, False, home)
            after = progress(observer, height + 2, 2)
            assert after["observer_only"] and after["member"] is None, after
            progress(incoming, height + 2, 2)
            hashes = [header(key, height + 2) for key in (observer, incoming)]
            write_json(args.output / "snapshot-headers.json", hashes)
            assert "header" in hashes[0] and hashes[0] == hashes[1], hashes
            restored = {"export": export, "ckb_live": live, "ckb_out_point": relay.point,
                        "ckb_inclusion": l1.prove_committed(relay.point["tx_hash"]),
                        "restored": result, "after": after, "rejected_pin": wrong.stderr,
                        "rejected_state": rejected.stderr, "snapshot_bytes": snapshot.stat().st_size}
        return {
            "replace": args.replace, "l1_transactions": {"registered_old": registered_old, "activated_old": activated_old,
                                "registered_incoming": registered_new, "activated_incoming": activated_new,
                                "exit_requested": retired, "increased_incoming": increased},
            "initial": initial, "observer_stopped": lag_start, "joined": joined, "committee_two": current,
            "after_retired_offline": while_old_offline, "caught_up": caught_up, "agreed": agreed,
            "restart_to_visibility_seconds": recovery_seconds,
            "handoffs": handoffs, "post_handoff_header": verified, "missing_handoff_rejected": wrong_epoch.stderr,
            "altered_handoff_rejected": bad_handoff.stderr,
            "ckb_checkpoints": checkpoints, "ckb_retirement": exit_report, "snapshot_restore": restored,
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
    parser.add_argument("--observer-extra-lag", type=int, default=0, help="additional blocks before restarting both offline replicas")
    parser.add_argument("--snapshot-restore", action="store_true", help="rebuild a fresh observer from a confirmed CKB checkpoint")
    parser.add_argument("--replace", action="store_true", help="replace the entire first committee, then increase power")
    parser.add_argument("--publish-checkpoint", action="store_true", help="publish native Jolteon checkpoints on the CKB devnet")
    parser.add_argument("--retire", action="store_true", help="retire and unbond the outgoing stake using the native checkpoint")
    parser.add_argument("--slash", action="store_true", help="slash the outgoing fixture validator using native vote evidence")
    parser.add_argument("--active-slash", action="store_true", help="freeze while the outgoing validator is still active")
    args = parser.parse_args()
    if args.observer_extra_lag < 0:
        parser.error("--observer-extra-lag must be nonnegative")
    if args.active_slash and (not args.slash or args.replace):
        parser.error("--active-slash exercises the join fixture with --slash")
    if args.retire and not args.publish_checkpoint:
        parser.error("--retire requires --publish-checkpoint")
    if args.snapshot_restore and not args.publish_checkpoint:
        parser.error("--snapshot-restore requires --publish-checkpoint")
    if args.slash and not args.retire:
        parser.error("--slash requires --retire")
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

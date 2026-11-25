#!/usr/bin/env python3
"""Drive real L1 bonds through native CometBFT committee transitions."""

import argparse
import base64
import concurrent.futures
from datetime import datetime
import hashlib
import json
from pathlib import Path
import struct
import subprocess

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import CKB, FEE, Fixtures, L1, binary, ckb_hash, hex_bytes, le, point, reject_cli, script_hash
from localnet import Rpc, comet_app_command, config_value, run, write_json
import checkpoint_localnet
import direct_checkpoint_localnet
from native_bridge_localnet import bridge_configuration, deploy as deploy_bridge, start, wait_for
import stake_load_localnet
from stake_localnet import CHAIN, DELAY, EMPTY, Scenario, guardian_group, mk_record, proof, signable


def signer(home):
    key = json.loads((home / "config/priv_validator_key.json").read_text())
    return Ed25519PrivateKey.from_private_bytes(base64.b64decode(key["priv_key"]["value"])[:32])


def conflicting_fixture_vote(commit, validator):
    """Use one actual CometBFT signature and one local fixture-only nil precommit."""
    def varint(value):
        data = bytearray()
        while value > 127:
            data.append((value & 127) | 128)
            value >>= 7
        data.append(value)
        return bytes(data)

    def field(tag, data):
        return bytes([tag]) + varint(len(data)) + data

    key = validator.public_key().public_bytes_raw()
    address = hashlib.sha256(key).digest()[:20].hex().upper()
    signed = next(sig for sig in commit["signatures"]
                  if sig["validator_address"] == address and sig["block_id_flag"] == 2)
    stamp, _, fraction = signed["timestamp"].removesuffix("Z").partition(".")
    seconds = int(datetime.fromisoformat(stamp + "+00:00").timestamp())
    nanos = int(fraction.ljust(9, "0"))
    timestamp = b"\x08" + varint(seconds) + (b"\x10" + varint(nanos) if nanos else b"")
    height, round_ = int(commit["height"]), commit["round"]
    prefix = b"\x08\x02\x11" + le(height, 8) + (b"\x19" + le(round_, 8) if round_ else b"")
    suffix = field(0x2a, timestamp) + field(0x32, CHAIN)
    block = bytes.fromhex(commit["block_id"]["hash"])
    part = commit["block_id"]["parts"]
    part_hash = bytes.fromhex(part["hash"])
    part_set = b"\x08" + varint(part["total"]) + field(0x12, part_hash)
    non_nil = prefix + field(0x22, field(0x0a, block) + field(0x12, part_set)) + suffix
    nil = prefix + suffix
    signature = base64.b64decode(signed["signature"])
    validator.public_key().verify(signature, varint(len(non_nil)) + non_nil)
    header = struct.pack("<Bqiqi", 2, height, round_, seconds, nanos)
    vote = header + b"\x01" + le(part["total"], 4) + block + part_hash + signature
    conflict = header + bytes(69) + validator.sign(varint(len(nil)) + nil)
    return vote + conflict, height, round_


def stake_round(args, l1, scenario, fixture):
    net = args.output / "net"
    run(args.comet, "testnet", "--v", "1", "--n", "1", "--o", net)
    homes = [net / "node0", net / "node1"]
    old, new = [signer(home) for home in homes]
    old_key, new_key = [key.public_key().public_bytes_raw() for key in (old, new)]
    owner = script_hash(fixture.standard)
    low, high = 250 * CKB, 400 * CKB

    def certificate(domain):
        return lambda previous, next_: proof(scenario.authority, signable(
            domain, scenario.network, scenario.genesis, scenario.registry_id, previous, next_))

    def register(key, amount, epoch, **validation):
        pub = key.public_key().public_bytes_raw()
        signature = lambda before, after: key.sign(signable(
            b"Stem CKB Stake registration\0", scenario.network, scenario.genesis, scenario.registry_id, before, after))
        return scenario.change(f"register-{epoch}", 0, pub,
                               mk_record(pub, amount, epoch, None, owner + bytes(48)), signature, **validation)

    def activate(pub, amount, epoch):
        metadata = owner + le(1 + epoch * 10, 8) + le(0, 8) + EMPTY
        scenario.change(f"activate-{epoch}", 3, pub, mk_record(pub, amount, epoch, None, metadata),
                        certificate(b"Stem CKB Stake activation\0"))
        return metadata

    rejected = {"underfunded": register(old, low, 0, expected="ValidationFailure", capacity_adjustment=-1)}
    registered = register(old, low, 0)
    rejected["duplicate_bond"] = register(old, low, 2, expected="ValidationFailure")
    original = activate(old_key, low, 0)
    rejected["incomplete_snapshot"] = scenario.observe("omitted", 0, missing=True)
    genesis = scenario.observe("initial", 0, count=3)
    verified, rejected["minority_snapshot"] = scenario.verify_genesis_snapshot(genesis)
    if verified["validators"] != [{"ed25519": hex_bytes(old_key), "power": 250}]:
        raise AssertionError(verified)
    incoming = register(new, high, 1)
    incoming_active = activate(new_key, high, 1)
    spec = {
        "chain_id": CHAIN.decode(), "network_id": hex_bytes(scenario.network), "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024, "transaction_cycles": 20_000_000,
                   "block_cycles": 400_000_000, "cell_reads": 1024, "cell_read_bytes": 64 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "stake": {"trust": "snapshot-trust.json", "observations": "genesis-observations.json",
                  "signing_window": 5, "limits": {"operations": 1, "bytes": 256 * 1024, "signatures": 8}},
    }
    vault = None
    if args.loadgen:
        args.xudt = None
        source = point(scenario.code["out_point"]["tx_hash"], 2)
        vault = deploy_bridge(args, l1, fixture, spec["network_id"], source)
        spec["bridge"], lane = bridge_configuration(args, l1, fixture, vault, None, spec["network_id"])
        for home in homes:
            config_value(home / "config/config.toml", "timeout_commit", '"3s"')
            config_value(home / "config/config.toml", "skip_timeout_commit", "false")
    spec_path = args.output / "stem-genesis.json"
    write_json(spec_path, spec)
    id_ = json.loads(run(args.binary, "genesis-hash", "--genesis", spec_path))
    comet = json.loads((homes[0] / "config/genesis.json").read_text())
    comet["chain_id"], comet["genesis_time"] = spec["chain_id"], "2025-01-01T00:00:00Z"
    comet["validators"][0]["power"] = "250"
    comet["app_state"] = {"stem_genesis": id_.removeprefix("0x")}
    for home in homes:
        write_json(home / "config/genesis.json", comet)
    nodes = []
    loader = None
    clients = [Rpc(args.comet_port + n) for n in range(2)]
    peers = [run(args.comet, "show-node-id", "--home", home) for home in homes]
    try:
        for n, home in enumerate(homes):
            app = args.output / f"app-{n}"
            nodes.append(start(args, [args.binary, comet_app_command(args.binary), "--home", app, "--genesis", spec_path, "--archive"], f"stem-{n}.log"))
            wait_for(lambda: (app / "abci.sock").is_socket(), f"Stem node {n}")
            nodes.append(start(args, [args.comet, "start", "--home", home, "--proxy_app", f"unix://{app / 'abci.sock'}",
                "--rpc.laddr", f"tcp://127.0.0.1:{args.comet_port + n}",
                "--p2p.laddr", f"tcp://127.0.0.1:{args.comet_port + 2 + n}",
                "--p2p.persistent_peers", f"{peers[1 - n]}@127.0.0.1:{args.comet_port + 3 - n}", "--p2p.pex=false"],
                f"comet-{n}.log"))

        def progress(height):
            wait_for(lambda: all(int(rpc.call("status")["sync_info"]["latest_block_height"]) >= height for rpc in clients),
                     f"both replicas at H{height}", seconds=45 if vault else 20)

        def header(rpc, height):
            reply = rpc.call("abci_query", path=json.dumps("/stem/header"), height=height)["response"]
            if reply["code"] != 0:
                raise AssertionError(reply)
            return base64.b64decode(reply["value"])

        def snapshot(epoch, parent, rotation=None):
            activation = 1 + 10 * epoch
            signing = activation - 6
            progress(signing)
            raw = header(clients[0], signing)
            anchor = hex_bytes(ckb_hash(b"Stem Header\0" + scenario.network + raw))
            votes = scenario.observe(f"epoch-{epoch}", epoch, parent, count=3, signing_anchor=anchor,
                                     rotation=rotation)
            attestations = args.output / f"epoch-{epoch}-votes.json"
            if epoch == 1:
                reject_cli(
                    "guardian already chose a different Stake Snapshot for this epoch",
                    args.binary, "stake", "observe", "--request", args.output / "epoch-1-observe.json",
                    "--l1-rpc", l1.url, "--journal", args.output / "stake-journal-0",
                    "--key-file", args.output / "stake-observer-0.key",
                    "--stem-signing-anchor", hex_bytes(bytes([19]) * 32),
                )
            if rotation:
                for subset in (votes[:3], votes[3:]):
                    write_json(attestations, subset)
                    tx = json.loads(run(args.binary, "stake", "snapshot", "--observations", attestations))
                    if clients[0].call("broadcast_tx_sync", tx=tx)["code"] == 0:
                        raise AssertionError("Stem accepted guardian rotation with a single quorum")
            write_json(attestations, votes)
            tx = json.loads(run(args.binary, "stake", "snapshot", "--observations", attestations))
            result = clients[0].call("broadcast_tx_commit", tx=tx)
            height = int(result["height"])
            if result["check_tx"]["code"] != 0 or not activation - 5 <= height <= activation - 2:
                raise AssertionError(result)
            return votes[0]["registry"], result

        def committee(height):
            result = clients[0].call("validators", height=height)
            return {base64.b64decode(member["pub_key"]["value"]): int(member["voting_power"])
                    for member in result["validators"]}

        workload = None
        if vault:
            progress(1)
            workload = stake_load_localnet.prepare(args, l1, fixture, vault, spec, spec_path, lane, clients[0])
        first, joined = snapshot(1, genesis[0]["registry"])
        if workload:
            loader = concurrent.futures.ThreadPoolExecutor(max_workers=1)
            measured = loader.submit(stake_load_localnet.measure, args, workload, nodes)
        progress(11)
        first_checkpoint = checkpoint_localnet.capture(args, clients[0], progress, 11) if args.checkpoint else None
        if committee(10) != {old_key: 250} or committee(11) != {old_key: 250, new_key: 400}:
            raise AssertionError("CKB stake did not take effect at the CometBFT H+2 boundary")
        previous_registry = point(scenario.registry_tx, 0)
        exit_requested = scenario.change("request-live-exit", 2, old_key, mk_record(old_key, low, 0, 2, original))
        current_record = original
        frozen = None
        if args.slash:
            commit = clients[0].call("commit", height=11)["signed_header"]["commit"]
            votes, height, round_ = conflicting_fixture_vote(commit, old)
            write_json(args.output / "double-vote-fixture.json", {
                "commit": commit, "validator": hex_bytes(old_key), "evidence": hex_bytes(votes),
            })
            penalty = ckb_hash(b"Stem CometBFT double vote\0" + scenario.network + le(len(CHAIN), 4)
                               + CHAIN + old_key + le(height, 8) + le(round_, 4) + b"\x02")
            current_record = original[:48] + penalty
            frozen_record = mk_record(old_key, low, 0, 2, current_record, frozen=True)
            rejected["modified_vote"] = scenario.change(
                "modified-double-vote", 6, old_key, frozen_record,
                lambda *_: votes[:-1] + bytes([votes[-1] ^ 1]), expected="ValidationFailure")
            frozen = scenario.change("freeze-canonical-votes", 6, old_key, frozen_record, lambda *_: votes, measure=True)
            rejected["replayed_vote"] = scenario.change(
                "repeat-freeze", 6, old_key, frozen_record, lambda *_: votes, expected="ValidationFailure")
            rejected["slash_before_retirement"] = scenario.change(
                "slash-before-retirement", 8, old_key, mk_record(old_key, 0, 0, 2, current_record, frozen=True),
                fee=low + FEE, expected="ValidationFailure")
        departing, arriving = scenario.authority, guardian_group(23)
        def rotate(before, next_):
            message = signable(b"Stem CKB Stake authority rotation\0", scenario.network,
                               scenario.genesis, scenario.registry_id, before, next_)
            return proof(departing, message) + proof(arriving, message)
        rotation_tx = scenario.change("rotate-active-guardians", 7, None, certificate=rotate, next_authority=arriving)
        scenario.authority = arriving
        parent, excluded = snapshot(2, first, (previous_registry, departing))

        def capture_marker(label):
            response = clients[0].call(
                "abci_query", path=json.dumps("/stem/stake-retirement"),
                data=hex_bytes(old_key), prove="true",
            )["response"]
            if response["code"] != 0:
                raise AssertionError(response)
            marker = json.loads(base64.b64decode(response["value"]))
            if marker["effective_height"] != 21 or not binary(marker["proof"]):
                raise AssertionError(marker)
            height = int(response["height"])
            marker_file = args.output / f"{label}-marker.json"
            header_file = args.output / f"{label}-header.json"
            write_json(marker_file, marker)
            write_json(header_file, hex_bytes(header(clients[0], height)))
            return height, marker, marker_file, header_file

        early_height, early_marker, early_file, early_header = capture_marker("before-retirement-boundary")
        if early_height >= 21:
            raise AssertionError("fixture missed the marker before exclusion")
        progress(23)
        if committee(20) != {old_key: 250, new_key: 400} or committee(21) != {new_key: 400}:
            raise AssertionError("CKB exit did not alter the expected CometBFT height")
        handoffs = args.output / "handoffs.json"
        transitions = [
            {"commit": clients[0].call("commit", height=h - 1),
             "members": clients[0].call("validators", height=h)["validators"]}
            for h in (11, 21)
        ]
        write_json(handoffs, transitions)
        final = args.output / "retirement-successor.json"
        write_json(final, clients[0].call("commit", height=22))
        projected = header(clients[0], 21)
        if projected != header(clients[1], 21):
            raise AssertionError("replicas disagree on the native retirement state")
        app_hash = ckb_hash(b"Stem AppHash\0" + scenario.network + le(21, 8)
                           + ckb_hash(b"Stem Header\0" + scenario.network + projected))
        validated = run(args.proof_verifier, "-genesis", homes[0] / "config/genesis.json", "-handoffs", handoffs,
                        "-commit", final, "-height", "21", "-app-hash", app_hash.hex())
        checkpoint_report = None
        second_checkpoint = checkpoint_localnet.capture(args, clients[0], progress, 23) if args.checkpoint else None
        increased_power = 450 * CKB
        increased = scenario.change("increase-active-validator", 1, new_key,
                                    mk_record(new_key, increased_power, 1, None, incoming_active))
        succeeding_registry, succeeding = snapshot(3, parent)
        progress(31)
        if committee(30) != {new_key: 400} or committee(31) != {new_key: 450}:
            raise AssertionError("CKB bond increase did not change power at the expected height")
        if args.checkpoint:
            checkpoint_report = checkpoint_localnet.publish(
                args, scenario, spec_path, homes[0] / "config/genesis.json",
                transitions, first_checkpoint, second_checkpoint,
                funding=vault["change_point"] if vault else None)
        relay = direct_checkpoint_localnet.Relay(
            args, l1, fixture, scenario.checkpoint, spec_path, homes[0] / "config/genesis.json",
            clients[0], directory="direct-checkpoints",
        )
        relay.advance(0, scenario.secp, probe=True)
        relay.advance(8, scenario.secp, probe=True)
        early_dep = relay.advance(early_height, scenario.secp)
        retired = current_record[:40] + le(21, 8) + current_record[48:]
        frozen_flag = bool(frozen)
        retired_record = mk_record(old_key, low, 0, 2, retired, frozen=frozen_flag)

        def build_witness(marker, proof_header):
            return [args.binary, "stake", "retirement-witness", "--genesis", spec_path,
                    "--validator", hex_bytes(old_key), "--header", proof_header, "--marker", marker]

        reject_cli("invalid or not yet effective Stem retirement marker", *build_witness(early_file, early_header))
        rejected["early_authenticated_marker"] = scenario.change(
            "retire-early", 4, old_key, retired_record, lambda *_: binary(early_marker["proof"]),
            dependencies=[early_dep], expected="error code 23")
        l1.mine(DELAY)
        rejected["withdraw_before_retirement"] = scenario.change(
            "withdraw-before-retirement", 5, old_key, mk_record(old_key, 0, 0, 2, current_record, True, frozen_flag),
            since=(1 << 63) | DELAY, expected="error code 23")

        marker_height, marker, marker_path, marker_header = capture_marker("effective-retirement")
        progress(marker_height + 1)
        commit = clients[0].call("commit", height=marker_height + 1)
        write_json(args.output / "effective-retirement-successor.json", commit)
        witness = binary(json.loads(run(*build_witness(marker_path, marker_header))))
        checkpoint = relay.advance(marker_height, scenario.secp)
        if len(relay.rejections) != 6 or len({h for p in relay.publications for h in p["validator_hashes"]}) < 4:
            raise AssertionError("incomplete direct checkpoint and committee regression")

        foreign = direct_checkpoint_localnet.Relay(
            args, l1, fixture, scenario.foreign_checkpoint, spec_path, homes[0] / "config/genesis.json",
            clients[0], directory="foreign-checkpoints",
        )
        foreign_dep = foreign.advance(marker_height, scenario.secp)
        rejected["foreign_checkpoint"] = scenario.change(
            "retire-foreign", 4, old_key, retired_record, lambda *_: witness,
            dependencies=[foreign_dep], expected="error code 23")
        rejected["missing_checkpoint"] = scenario.change(
            "retire-missing-checkpoint", 4, old_key, retired_record, lambda *_: witness,
            expected="error code 23")
        rejected["tampered_path"] = scenario.change(
            "retire-bad-path", 4, old_key, retired_record, lambda *_: witness[:-1] + bytes([witness[-1] ^ 1]),
            dependencies=[checkpoint], expected="error code 23")
        legacy_certificate = certificate(b"Stem CKB Stake retirement\0")
        rejected["guardian_certificate_without_marker"] = scenario.change(
            "retire-by-guardians", 4, old_key, retired_record, legacy_certificate,
            dependencies=[checkpoint], expected="error code 23")
        retirement = scenario.change("retire-excluded", 4, old_key, retired_record, lambda *_: witness,
                                     dependencies=[checkpoint], measure=True, relayer=True)
        rejected["replayed_marker"] = scenario.change(
            "retire-again", 4, old_key, retired_record, lambda *_: witness,
            dependencies=[checkpoint], expected="error code 23")
        slash = None
        if frozen_flag:
            l1.mine(DELAY)
            rejected["withdraw_before_slash"] = scenario.change(
                "withdraw-before-slash", 5, old_key, mk_record(old_key, 0, 0, 2, retired, True, True),
                since=(1 << 63) | DELAY, expected="error code 23")
            slashed = mk_record(old_key, 0, 0, 2, retired, frozen=True)
            rejected["unfunded_penalty"] = scenario.change(
                "unfunded-penalty", 8, old_key, slashed, expected="error code 23")
            slash = scenario.change("finalize-slash", 8, old_key, slashed, fee=low + FEE, measure=True)
        final_record = mk_record(old_key, 0, 0, 2, retired, True, frozen_flag)
        rejected["unbond_delay"] = scenario.change(
            "early-unbonding", 5, old_key, final_record, since=(1 << 63) | DELAY, expected="Immature")
        l1.mine(DELAY)
        rejected["unauthorized_unbonding"] = scenario.change(
            "unbond-by-relayer", 5, old_key, final_record,
            since=(1 << 63) | DELAY, expected="error code 23", relayer=True)
        released = scenario.change("unbond-excluded", 5, old_key, final_record,
                                   since=(1 << 63) | DELAY, measure=True)
        rejected["reregistration"] = register(old, low, 4, expected="error code 23")
        if (old_key in scenario.records or old_key not in scenario.retired
                or script_hash(fixture.friend_lock) == owner):
            raise AssertionError("withdrawal did not retire the key")
        direct_checkpoint = relay.report()
        stake_transition = {
            "early_marker_height": early_height, "marker_height": marker_height, "effective_height": marker["effective_height"],
            "foreign_checkpoint": foreign.report(), "retirement_inclusion": l1.prove_committed(retirement),
            "frozen": frozen, "slash": slash, "slashed_shannons": low if slash else 0,
            "slash_inclusion": l1.prove_committed(slash) if slash else None,
            "measurements": scenario.measurements, "rejections": rejected,
            "retirement_fee_payer": hex_bytes(script_hash(fixture.friend_lock)), "stake_owner": hex_bytes(owner),
        }
        report = {"initial_registration": registered, "incoming_registration": incoming, "stake_join": joined,
                "exit_requested": exit_requested, "stake_exit": excluded, "retirement": retirement,
                "unbonded": released, "unbonded_shannons": 0 if slash else low, "l1_proof": l1.prove_committed(released),
                "retirement_transition": stake_transition,
                "committee_proof": validated, "checkpoint": checkpoint_report, "direct_checkpoint": direct_checkpoint,
                "guardian_rotation": rotation_tx, "next_guardian_snapshot": succeeding,
                "increased_bond": increased, "increased_power_at_height_31": 450,
                "guardian_conflicting_epoch_rejected": True,
                "load": measured.result(timeout=90) if loader else None}
        write_json(args.output / "native-stake-pre-reorg.json", report)
        height = int(succeeding_registry["height"])
        observed = succeeding_registry["block_hash"]
        l1.call("truncate", l1.call("get_block_hash", hex(height - 1)))
        l1.mine(3)
        replacement = l1.call("get_block_hash", hex(height))
        if replacement == observed:
            raise AssertionError("failure to reorganize the observed CKB anchor")
        observer = scenario.authority[0].public_key().public_bytes_raw().hex()
        reason = reject_cli(
            "previous L1 anchor was reorganized",
            args.binary, "stake", "observe", "--request", args.output / "epoch-3-observe.json",
            "--l1-rpc", l1.url, "--journal", args.output / "stake-journal-0",
            "--key-file", args.output / f"stake-observer-{observer}.key",
        )
        report["reorganized_ckb_anchor"] = {"observed": observed, "replacement": replacement,
                                            "height": height, "rejected": reason}
        return report
    finally:
        for client in clients:
            client.close()
        for child in reversed(nodes):
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        if loader:
            loader.shutdown(wait=True, cancel_futures=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "comet", "stake-registry", "proof-verifier", "direct-checkpoint", "output"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True)
    for name in ("loadgen", "vault", "deposit-lock", "native-lock", "wrapped"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    parser.add_argument("--checkpoint", type=lambda path: Path(path).resolve())
    parser.add_argument("--slash", action="store_true", help="freeze and slash an actual fixture validator")
    parser.add_argument("--l1-port", type=int, default=43300)
    parser.add_argument("--comet-port", type=int, default=43310)
    args = parser.parse_args()
    if args.loadgen and not all((args.vault, args.deposit_lock, args.native_lock, args.wrapped)):
        parser.error("funded stake load requires --vault, --deposit-lock, --native-lock and --wrapped")
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    try:
        fixture = Fixtures(args)
        result = stake_round(args, l1, Scenario(args, l1, fixture), fixture)
        write_json(args.output / "native-stake-report.json", result)
        print(result["committee_proof"], f"unbonded {result['unbonded_shannons']} shannons")
        if result["checkpoint"]:
            print(f"published CKB checkpoints at Stem heights {[item['height'] for item in result['checkpoint']['publications']]}")
        if result["direct_checkpoint"]:
            print(f"verified direct CKB checkpoints at Stem heights {[item['height'] for item in result['direct_checkpoint']['publications']]}")
        if result["load"]:
            print(json.dumps(result["load"], indent=2))
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

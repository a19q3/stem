#!/usr/bin/env python3
"""Drive real L1 bonds through native CometBFT committee transitions."""

import argparse
import base64
import json
from pathlib import Path
import secrets
import subprocess
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import CKB, Fixtures, L1, binary, ckb_hash, hex_bytes, le, script_hash
from localnet import Rpc, run, write_json
from native_bridge_localnet import start, wait_for
from stake_localnet import CHAIN, DELAY, EMPTY, Scenario, mk_record, proof, signable


def signer(home):
    key = json.loads((home / "config/priv_validator_key.json").read_text())
    return Ed25519PrivateKey.from_private_bytes(base64.b64decode(key["priv_key"]["value"])[:32])


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

    def register(key, amount, epoch):
        pub = key.public_key().public_bytes_raw()
        signature = lambda before, after: key.sign(signable(
            b"Fern CKB Stake registration\0", scenario.network, scenario.genesis, scenario.registry_id, before, after))
        return scenario.change(f"register-{epoch}", 0, pub,
                               mk_record(pub, amount, epoch, None, owner + bytes(48)), signature)

    def activate(pub, amount, epoch):
        metadata = owner + le(1 + epoch * 10, 8) + le(0, 8) + EMPTY
        scenario.change(f"activate-{epoch}", 3, pub, mk_record(pub, amount, epoch, None, metadata),
                        certificate(b"Fern CKB Stake activation\0"))
        return metadata

    registered = register(old, low, 0)
    original = activate(old_key, low, 0)
    genesis = scenario.observe("initial", 0, count=3)
    verified, _ = scenario.verify_genesis_snapshot(genesis)
    if verified["validators"] != [{"ed25519": hex_bytes(old_key), "power": 250}]:
        raise AssertionError(verified)
    incoming = register(new, high, 1)
    activate(new_key, high, 1)
    spec = {
        "chain_id": CHAIN.decode(), "network_id": hex_bytes(scenario.network), "genesis_time_ms": 1_735_689_600_000,
        "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
        "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024, "transaction_cycles": 20_000_000,
                   "block_cycles": 400_000_000, "cell_reads": 1024, "cell_read_bytes": 64 * 1024 * 1024,
                   "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
        "stake": {"trust": "snapshot-trust.json", "observations": "genesis-observations.json",
                  "signing_window": 5, "limits": {"operations": 1, "bytes": 256 * 1024, "signatures": 8}},
    }
    spec_path = args.output / "fern-genesis.json"
    write_json(spec_path, spec)
    id_ = json.loads(run(args.binary, "genesis-hash", "--genesis", spec_path))
    comet = json.loads((homes[0] / "config/genesis.json").read_text())
    comet["chain_id"], comet["genesis_time"] = spec["chain_id"], "2025-01-01T00:00:00Z"
    comet["validators"][0]["power"] = "250"
    comet["app_state"] = {"fern_genesis": id_.removeprefix("0x")}
    for home in homes:
        write_json(home / "config/genesis.json", comet)
    nodes = []
    clients = [Rpc(args.comet_port + n) for n in range(2)]
    peers = [run(args.comet, "show-node-id", "--home", home) for home in homes]
    try:
        for n, home in enumerate(homes):
            app = args.output / f"app-{n}"
            nodes.append(start(args, [args.binary, "run", "--home", app, "--genesis", spec_path], f"fern-{n}.log"))
            wait_for(lambda: (app / "abci.sock").is_socket(), f"Fern node {n}")
            nodes.append(start(args, [args.comet, "start", "--home", home, "--proxy_app", f"unix://{app / 'abci.sock'}",
                "--rpc.laddr", f"tcp://127.0.0.1:{args.comet_port + n}",
                "--p2p.laddr", f"tcp://127.0.0.1:{args.comet_port + 2 + n}",
                "--p2p.persistent_peers", f"{peers[1 - n]}@127.0.0.1:{args.comet_port + 3 - n}", "--p2p.pex=false"],
                f"comet-{n}.log"))

        def progress(height):
            wait_for(lambda: all(int(rpc.call("status")["sync_info"]["latest_block_height"]) >= height for rpc in clients),
                     f"both replicas at H{height}")

        def header(rpc, height):
            reply = rpc.call("abci_query", path=json.dumps("/fern/header"), height=height)["response"]
            if reply["code"] != 0:
                raise AssertionError(reply)
            return base64.b64decode(reply["value"])

        def snapshot(epoch, parent):
            activation = 1 + 10 * epoch
            signing = activation - 6
            progress(signing)
            raw = header(clients[0], signing)
            anchor = hex_bytes(ckb_hash(b"Fern Header\0" + scenario.network + raw))
            votes = scenario.observe(f"epoch-{epoch}", epoch, parent, count=3, signing_anchor=anchor)
            attestations = args.output / f"epoch-{epoch}-votes.json"
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

        first, joined = snapshot(1, genesis[0]["registry"])
        progress(11)
        if committee(10) != {old_key: 250} or committee(11) != {old_key: 250, new_key: 400}:
            raise AssertionError("CKB stake did not take effect at the CometBFT H+2 boundary")
        exit_requested = scenario.change("request-live-exit", 2, old_key, mk_record(old_key, low, 0, 2, original))
        _, excluded = snapshot(2, first)
        progress(23)
        if committee(20) != {old_key: 250, new_key: 400} or committee(21) != {new_key: 400}:
            raise AssertionError("CKB exit did not alter the expected CometBFT height")
        handoffs = args.output / "handoffs.json"
        write_json(handoffs, [
            {"commit": clients[0].call("commit", height=h - 1),
             "members": clients[0].call("validators", height=h)["validators"]}
            for h in (11, 21)
        ])
        final = args.output / "retirement-successor.json"
        write_json(final, clients[0].call("commit", height=22))
        projected = header(clients[0], 21)
        if projected != header(clients[1], 21):
            raise AssertionError("replicas disagree on the native retirement state")
        app_hash = ckb_hash(b"Fern AppHash\0" + scenario.network + le(21, 8)
                           + ckb_hash(b"Fern Header\0" + scenario.network + projected))
        validated = run(args.proof_verifier, "-genesis", homes[0] / "config/genesis.json", "-handoffs", handoffs,
                        "-commit", final, "-height", "21", "-app-hash", app_hash.hex())
        retired = owner + le(1, 8) + le(21, 8) + EMPTY
        retirement = scenario.change("retire-excluded", 4, old_key, mk_record(old_key, low, 0, 2, retired),
                                     certificate(b"Fern CKB Stake retirement\0"))
        l1.mine(DELAY)
        released = scenario.change("unbond-excluded", 5, old_key, mk_record(old_key, 0, 0, 2, retired, True),
                                   since=(1 << 63) | DELAY)
        return {"initial_registration": registered, "incoming_registration": incoming, "stake_join": joined,
                "exit_requested": exit_requested, "stake_exit": excluded, "retirement": retirement,
                "unbonded": released, "unbonded_shannons": low, "l1_proof": l1.prove_committed(released),
                "committee_proof": validated}
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "comet", "stake-registry", "proof-verifier", "output"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, default=43300)
    parser.add_argument("--comet-port", type=int, default=43310)
    args = parser.parse_args()
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    try:
        fixture = Fixtures(args)
        result = stake_round(args, l1, Scenario(args, l1, fixture), fixture)
        write_json(args.output / "native-stake-report.json", result)
        print(result["committee_proof"], f"unbonded {result['unbonded_shannons']} shannons")
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

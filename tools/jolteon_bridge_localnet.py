#!/usr/bin/env python3
"""Exercise the native checkpoint and permissionless CKB and xUDT payouts."""

import argparse
import base64
import json
from pathlib import Path
import secrets
import struct
import subprocess

from bridge_localnet import Fixtures, L1, binary, ckb_hash, hex_bytes
from jolteon_checkpoint_localnet import Relay
from jolteon_localnet import FINALITY, HEADER, QUERY, STATUS, SUBMIT, evidence_keys, rpc, write_configuration
from localnet import write_json
from native_bridge_localnet import (bridge_configuration, deploy, deploy_token, deposit_and_mint, lane_proof,
                                    reconcile_backing, start, stem_id, vault_operations, verify_l1_output,
                                    wait_for, withdraw_and_settle, xudt_round_trip)


class Client:
    """Adapter for the shared bridge fixture, using only finalized native queries."""

    def __init__(self, args, port, network):
        self.args, self.port, self.network = args, port, network
        self.locations = {}

    def status(self):
        return rpc(self.port, STATUS)

    def header(self, height):
        return rpc(self.port, HEADER, struct.pack("<Q", height))

    def finality(self, height):
        return rpc(self.port, FINALITY, struct.pack("<Q", height))

    def query(self, path, *, data="0x", height=0, prove=False):
        query = json.dumps(dict(path=path, data=data, height=int(height), prove=prove)).encode()
        reply = rpc(self.port, QUERY, query)
        return reply

    def call(self, method, **params):
        if method == "status":
            return {"sync_info": {"latest_block_height": self.status()["height"]}}
        if method == "abci_query":
            path = json.loads(params["path"])
            reply = self.query(path, data=params.get("data", "0x"),
                               height=params.get("height", 0), prove=params.get("prove") == "true")
            if "error" in reply:
                return {"response": {"code": 1, "log": reply["error"]}}
            return {"response": {"code": 0, "height": reply["height"],
                    "value": base64.b64encode(bytes.fromhex(reply["value"])).decode("ascii")}}
        if method == "tx":
            return self.locations[params["hash"].removeprefix("0x")]
        if method not in ("broadcast_tx_sync", "broadcast_tx_commit"):
            raise ValueError(f"unsupported bridge fixture request {method}")
        first_height = self.status()["height"] + 1
        admitted = rpc(self.port, SUBMIT, binary(params["tx"]))
        if "error" in admitted:
            return {"code": 1, "log": admitted["error"]}
        if method == "broadcast_tx_sync":
            return {"code": 0, "hash": admitted["admitted"]}
        id_ = stem_id(self.args, self.network, params["tx"]).removeprefix("0x")
        next_height = first_height

        def committed():
            nonlocal next_height
            latest = self.status()["height"]
            while next_height <= latest:
                result = self.query("/stem/transactions", height=next_height)
                ids = json.loads(bytes.fromhex(result["value"]))
                if id_ in ids:
                    self.locations[id_] = {"index": ids.index(id_), "height": next_height}
                    return next_height
                next_height += 1
            return None

        height = wait_for(committed, "Jolteon finalized bridge transaction", seconds=35)
        return {"height": height, "hash": id_, "check_tx": {"code": 0}}


def bridge_round(args, l1, fixture, children):
    network = hex_bytes(ckb_hash(b"Stem native Jolteon bridge fixture\0" + secrets.token_bytes(32)))
    vault = deploy(args, l1, fixture, network)
    token = deploy_token(args, l1, fixture, vault) if args.xudt else None
    bridge, lane = bridge_configuration(args, l1, fixture, vault, token, network)
    keys = evidence_keys(args)
    members = [{"ed25519": hex_bytes(key), "power": 1} for key, _ in keys]
    spec = {"chain_id": "stem-jolteon-ckb-local", "network_id": network, "genesis_time_ms": 1_735_689_600_000,
            "capacity": {"bond_per_byte": 1_000_000, "state_overhead": 80},
            "limits": {"transaction_count": 64, "transaction_bytes": 2 * 1024 * 1024,
                       "transaction_cycles": 20_000_000, "block_cycles": 400_000_000, "cell_reads": 1024,
                       "cell_read_bytes": 64 * 1024 * 1024, "cell_write_bytes": 4 * 1024 * 1024, "header_dep_age": 64},
            "development_validators": members, "bridge": bridge}
    spec_path = args.output / "stem.json"
    write_json(spec_path, spec)
    ports = list(range(args.port, args.port + len(keys)))
    configuration = write_configuration(args.output, [key for key, _ in keys], ports, 300, 1600)
    clients = [Client(args, port, network) for port in ports]
    homes = [args.output / f"node-{port}" for port in ports]
    for i, (_, key) in enumerate(keys):
        command = [args.binary, "run", "--home", homes[i], "--genesis", spec_path, "--configuration",
                   configuration, "--key-file", key, "--initialize"]
        if i == 0:
            command.append("--archive")
        children.append(start(args, command, f"jolteon-{i}.log"))
    client = clients[0]
    wait_for(lambda: all(peer.status()["height"] >= 2 for peer in clients), "native bridge replicas")
    relay = Relay(args, l1, fixture, vault["anchor"], homes[0] / "db", spec_path, members, client)
    relay.advance(1, vault["secp"], probe=True)
    mint = deposit_and_mint(args, l1, fixture, vault, spec, spec_path, lane, client)
    write_json(args.output / "native-mint-report.json", mint)
    payout = withdraw_and_settle(args, l1, fixture, vault, spec, spec_path, lane, mint, client, relay)
    report = {"mint": mint, "withdrawal": payout}
    if token:
        token["custody_cell"] = vault["custody_cell"]
        report["xudt"] = xudt_round_trip(args, l1, fixture, token, spec, spec_path, mint, payout, client, relay)
        report["vault_operations"] = vault_operations(args, l1, fixture, vault, token, lane, payout,
                                                     report["xudt"], spec_path, client)
    report["checkpoint"] = relay.report()
    report["backing"] = reconcile_backing(client, report)
    report["authenticated_ckb_outputs"] = verify_l1_output(args, l1, payout, token is not None)
    report["lane_proofs"] = {lane: lane_proof(client, lane)}
    if token:
        asset_lane = report["xudt"]["lane"]
        report["lane_proofs"][asset_lane] = lane_proof(client, asset_lane)
    height = client.status()["height"]
    wait_for(lambda: all(peer.status()["height"] >= height for peer in clients), "all bridge replicas finalized")
    header = client.header(height)
    assert all(peer.header(height) == header for peer in clients)
    report["agreed"] = header
    report["replicas"] = [peer.status() for peer in clients]
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "l1", "vault", "deposit-lock", "native-lock", "wrapped", "direct-checkpoint", "output"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(), required=True)
    for name in ("xudt", "xudt-gate"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve())
    parser.add_argument("--validators", type=int, default=4)
    parser.add_argument("--port", type=int, default=22200)
    parser.add_argument("--l1-port", type=int, default=22210)
    args = parser.parse_args()
    if bool(args.xudt) != bool(args.xudt_gate):
        parser.error("--xudt and --xudt-gate must be specified together")
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    children = []
    try:
        report = bridge_round(args, l1, Fixtures(args), children)
        write_json(args.output / "jolteon-bridge-report.json", report)
        print(f"Verified CKB payout {report['withdrawal']['l1_payout']} and agreed height {report['agreed']['height']}")
        if args.xudt:
            print(f"Verified xUDT payout {report['xudt']['withdrawal']['l1_payout']}")
    finally:
        for child in children:
            if child.poll() is None:
                child.terminate()
        for child in children:
            try:
                child.wait(timeout=8)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        l1.stop()


if __name__ == "__main__":
    main()

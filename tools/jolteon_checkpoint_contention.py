"""Compete on one CKB checkpoint and then relay an authenticated workload."""

from concurrent.futures import ThreadPoolExecutor
import json
from subprocess import TimeoutExpired
import time
import tomllib

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (
    CKB, FAUCET_ARG, FEE, CkbRpc, Fixtures, binary, ckb_hash, dependency,
    hex_bytes, le, output, point, transaction, wallet_group,
)
from jolteon_bridge_localnet import Client
from jolteon_checkpoint_localnet import PublisherFixture, Relay
from localnet import process_resources, write_json
from native_bridge_localnet import start, wait_for
from stake_localnet import witness


def import_setup_blocks(origin, peer):
    # Import setup through normal CKB block verification before connecting.
    tip_height = int(origin.call("get_tip_block_number"), 16)
    for height in range(1, tip_height + 1):
        block = origin.call("get_block_by_number", hex(height))
        block_hash = block["header"].pop("hash")
        for uncle in block["uncles"]:
            uncle["header"].pop("hash")
        for tx in block["transactions"]:
            tx.pop("hash")
        assert peer.call("submit_block", "0", block) == block_hash
    return tip_height


def connect(origin, peer, port):
    node = peer.call("local_node_info")
    origin.call("add_node", node["node_id"], f"/ip4/127.0.0.1/tcp/{port + 1}")
    wait_for(lambda: origin.call("get_peers"), "CKB relay peer connection", seconds=20)
    tip = origin.call("get_tip_header")["hash"]
    wait_for(lambda: peer.call("get_tip_header")["hash"] == tip, "CKB peer syncing origin tip", seconds=30)
    return tip


def default_pool_policies(*nodes):
    policies = [tomllib.loads((node.home / "ckb.toml").read_text())["tx_pool"] for node in nodes]
    assert all(policy["max_tx_verify_cycles"] == 70_000_000 and policy["min_fee_rate"] == 1_000
               for policy in policies), policies
    return policies


def archived_replica(args, members):
    directory = args.stem_run
    configuration = directory / "replicas.json"
    peer = json.loads(configuration.read_text())["peers"][-1]
    key = next(path for path in directory.glob("validator-*.key")
               if Ed25519PrivateKey.from_private_bytes(path.read_bytes()).public_key().public_bytes_raw().hex()
               == peer["key"])
    home = directory / f"node{len(members) - 1}"
    command = [args.binary, "run", "--home", home, "--configuration", configuration,
               "--genesis", directory / "genesis.json", "--key-file", key, "--archive"]
    return start(args, command, "archive-reader.log"), home / "db", int(peer["address"].rsplit(":", 1)[1])


def status(node, tx_hash):
    entry = node.call("get_transaction", tx_hash, "0x1", False)
    return entry["tx_status"] if entry else {"status": "unknown"}


def waiting(node, tx_hash):
    state = status(node, tx_hash)
    assert state["status"] != "rejected", state
    return state if state["status"] in ("pending", "proposed") else None


def relay_workload(args, origin, peer, publisher, height, background):
    started = time.perf_counter()

    def submit(signed):
        rpc = CkbRpc(args.l1_port)
        sent = time.perf_counter()
        try:
            result = rpc.call("send_transaction", signed["json"], "passthrough")
            assert result == signed["hash"], result
        finally:
            rpc.close()
        return {"transaction": result, "submission_ms": (sent - started) * 1000,
                "accepted_ms": (time.perf_counter() - started) * 1000}

    def publish():
        sent = time.perf_counter()
        result = publisher.tick("follow-winner-with-independent-fee-input", height)
        assert (result["status"], result["stem_height"]) == ("submitted", height), result
        return {**result, "submission_ms": (sent - started) * 1000,
                "accepted_ms": (time.perf_counter() - started) * 1000}

    records, samples = [], []
    try:
        with ThreadPoolExecutor(max_workers=len(background) + 1) as workers:
            jobs = [(origin, workers.submit(publish))] + [(peer, workers.submit(submit, tx)) for tx in background]
            expires = time.monotonic() + 40
            while jobs or any("relayed_ms" not in record for record, _ in records):
                ready = [(node, job) for node, job in jobs if job.done()]
                jobs = [(node, job) for node, job in jobs if (node, job) not in ready]
                for receiving, task in ready:
                    record = task.result()
                    record["receiver"] = "origin" if receiving is origin else "peer"
                    records.append((record, receiving))
                for record, receiving in records:
                    if "relayed_ms" not in record and (seen := waiting(receiving, record["transaction"])):
                        record["relayed_ms"] = (time.perf_counter() - started) * 1000
                        record["receiver_status"] = seen
                        record["receiver_pool_detail"] = receiving.call("get_pool_tx_detail_info", record["transaction"])
                samples.append({
                    "elapsed_ms": (time.perf_counter() - started) * 1000,
                    "origin": origin.call("tx_pool_info"), "peer": peer.call("tx_pool_info"),
                })
                if time.monotonic() >= expires:
                    raise TimeoutError(f"checkpoint relay workload did not converge: {[record for record, _ in records]}")
                if jobs or any("relayed_ms" not in record for record, _ in records):
                    time.sleep(0.025)
    finally:
        write_json(args.output / "pool-samples.json", samples)
        write_json(args.output / "relayed-workload.json", [record for record, _ in records])
    return [record for record, _ in records]


def run_contention(args, origin, peer):
    policies = default_pool_policies(origin, peer)
    cycle_limits = [int(node.call("get_consensus")["max_block_cycles"], 16) for node in (origin, peer)]
    assert cycle_limits[0] == cycle_limits[1], cycle_limits
    if args.ckb_block_cycles is not None:
        assert cycle_limits[0] == args.ckb_block_cycles, cycle_limits
    fixture = Fixtures(args)
    verified = json.loads((args.stem_run / "jolteon-report.json").read_text())["checkpoint_proofs"]
    if len(verified) < 3:
        raise ValueError("collect at least three exact-height live proofs using --checkpoint-proofs")
    first, competing, following = verified[-3:]
    spec = args.stem_run / "genesis.json"
    parameters = json.loads(spec.read_text())
    members = parameters["development_validators"]
    genesis = origin.call("get_block_by_number", "0x0")
    assert genesis["header"]["hash"] == peer.call("get_block_hash", "0x0")
    initial, dependencies = genesis["transactions"][:2]
    faucet_index, funds = next((i, out) for i, out in enumerate(initial["outputs"]) if out["lock"]["args"] == FAUCET_ARG)
    secp = dependency(point(dependencies["hash"], 0), "dep_group")
    code = args.direct_checkpoint.read_bytes()
    occupied = (len(code) + 200) * CKB
    seed_capacity = 8_000 * CKB
    background_count = args.background_checkpoints
    remainder = int(funds["capacity"], 16) - occupied - (background_count + 2) * seed_capacity - FEE
    outputs = [output(occupied, fixture.standard), output(seed_capacity, fixture.standard),
               output(seed_capacity, fixture.owner_lock)]
    outputs.extend(output(seed_capacity, fixture.standard) for _ in range(background_count))
    outputs.append(output(remainder, fixture.standard))
    setup = transaction([point(initial["hash"], faucet_index)], outputs,
                        [hex_bytes(code)] + ["0x"] * (len(outputs) - 1), [secp])
    deployed = origin.send(fixture.sign("deploy-checkpoint", setup, [wallet_group(0, [fixture.faucet])]))

    def anchor(index):
        return {"hash": hex_bytes(ckb_hash(code)), "code": dependency(point(deployed, 0)),
                "id": ckb_hash(le(0, 8) + binary(deployed) + le(index, 4) + le(0, 8)),
                "seed": point(deployed, index), "genesis": genesis["header"]["hash"],
                "network": parameters["network_id"]}

    reader, archive, port = archived_replica(args, members)
    try:
        client = Client(args, port, parameters["network_id"])
        wait_for(lambda: client.status()["height"] >= following["height"], "archive RPC restarted", seconds=30)
        primary = Relay(args, origin, fixture, anchor(1), archive, spec, members, client)
        primary.advance(first["height"], secp, proof_file=args.stem_run / first["proof_file"])
        bootstrap = primary.point["tx_hash"]
        origin.mine(2)
        setup_height = import_setup_blocks(origin, peer)
        assert not origin.call("get_peers") and not peer.call("get_peers"), "competition requires separate pools"
        publisher = PublisherFixture(args, peer, fixture, primary, secp, name="competing-publisher")
        publisher.config.update(fee_key=str(fixture.owner), update_funding=point(deployed, 2))
        adopted = publisher.tick("pin-shared-parent", adopt=[bootstrap])
        assert adopted["checkpoint"] == primary.point, adopted

        # Build and admit both valid spends on the same canonical parent.
        ahead = primary.bundle(competing["height"], proof_file=args.stem_run / competing["proof_file"])
        previous = primary.point
        capacity = int(origin.call("get_live_cell", primary.funding, True)["cell"]["output"]["capacity"], 16)
        tx = transaction([previous, primary.funding],
                         [output(1000 * CKB, primary.lock, primary.type), output(capacity - FEE, fixture.friend_lock)],
                         [ahead["tip"], "0x"], [primary.anchor["code"], secp])
        tx["witnesses"] = [witness(binary(ahead["witness"]))]
        winner = fixture.sign("origin-contender", tx, [wallet_group(1, [fixture.friend])])
        winner_cycles = int(origin.call("estimate_cycles", winner["json"])["cycles"], 16)
        competing_hash = winner["hash"]
        assert origin.call("send_transaction", winner["json"], "passthrough") == competing_hash
        loser = publisher.tick("peer-admits-conflicting-spend", competing["height"])
        assert loser["status"] == "submitted" and loser["transaction"] != competing_hash, loser
        losing_hash = loser["transaction"]
        loser_tx = peer.call("get_transaction", losing_hash, "0x2", False)["transaction"]
        assert [item["previous_output"] for item in loser_tx["inputs"]] == [previous, point(deployed, 2)], loser_tx
        assert loser_tx["outputs_data"][0] == ahead["tip"], loser_tx
        pools = {
            "origin_winner": waiting(origin, competing_hash), "peer_loser": waiting(peer, losing_hash),
            "peer_winner": status(peer, competing_hash), "origin_loser": status(origin, losing_hash),
            "origin_detail": origin.call("get_pool_tx_detail_info", competing_hash),
            "peer_detail": peer.call("get_pool_tx_detail_info", losing_hash),
            "peer_transaction": peer.call("get_transaction", losing_hash, "0x0", False)["transaction"],
        }
        assert min(winner_cycles, loser["estimated_cycles"]) > policies[0]["max_tx_verify_cycles"]
        assert pools["origin_winner"] and pools["peer_loser"], pools
        assert pools["peer_winner"]["status"] == pools["origin_loser"]["status"] == "unknown", pools
        write_json(args.output / "competing-pools.json", pools)

        def mined():
            origin.mine(1)
            return status(origin, competing_hash)["status"] == "committed"

        wait_for(mined, "origin mined the competing checkpoint", seconds=20)
        origin.mine(2)
        divergence = {"origin": origin.call("get_tip_header"), "peer": peer.call("get_tip_header")}
        started = time.perf_counter()
        sync_tip = connect(origin, peer, args.relay_port)
        sync_ms = (time.perf_counter() - started) * 1000
        accepted = publisher.tick("adopt-canonical-competing-spend", adopt=[competing_hash])
        assert accepted["checkpoint"] == point(competing_hash, 0), accepted
        def invalidated():
            state = status(peer, losing_hash)
            assert state["status"] != "committed", state
            return state if state["status"] in ("rejected", "unknown") else None

        losing_state = wait_for(invalidated, "CKB peer dropped its conflicting spend", seconds=15)
        inclusion = origin.prove_committed(competing_hash)
        assert peer.prove_committed(competing_hash) == inclusion

        # Publish a bounded batch with different Type IDs and the same real QC.
        measured = []
        for i in range(background_count):
            relay = Relay(args, origin, fixture, anchor(i + 3), archive, spec, members, client,
                          directory=f"background-checkpoint-{i}")
            proof = relay.bundle(following["height"], proof_file=args.stem_run / following["proof_file"])
            capacity = seed_capacity - 1000 * CKB - FEE
            tx = transaction([relay.anchor["seed"]],
                             [output(1000 * CKB, relay.lock, relay.type), output(capacity, fixture.friend_lock)],
                             [proof["tip"], "0x"], [relay.anchor["code"], secp])
            tx["witnesses"] = [witness(binary(proof["witness"]))]
            measured.append(fixture.sign(f"background-checkpoint-{i}", tx, [wallet_group(0, [fixture.faucet])]))
        nodes = {"origin": origin, "peer": peer}
        before_pool = {name: node.call("tx_pool_info") for name, node in nodes.items()}
        before_resources = {name: process_resources(node.process.pid) for name, node in nodes.items()}
        relayed = relay_workload(args, origin, peer, publisher, following["height"], measured)
        after_resources = {name: process_resources(node.process.pid) for name, node in nodes.items()}
        after_pool = {name: node.call("tx_pool_info") for name, node in nodes.items()}
        for record in relayed:
            publisher.mine_inclusion(record["transaction"])
        successor = next(record for record in relayed if record["receiver"] == "origin")
        confirmed = publisher.confirm("confirm-canonical-follow-up", successor)
        resumed = peer.call("get_transaction", successor["transaction"], "0x2", True)["transaction"]
        assert [i["previous_output"] for i in resumed["inputs"]] == [point(competing_hash, 0), point(deployed, 2)], resumed
        for record in relayed:
            tx_hash = record["transaction"]
            wait_for(lambda: status(origin, tx_hash)["status"] == "committed", "origin receiving the peer block", seconds=20)
            record["inclusion"] = peer.prove_committed(tx_hash)
            assert origin.prove_committed(tx_hash) == record["inclusion"]

        report = {"committee_size": len(members), "proofs": verified, "background_checkpoints": background_count,
                  "pool_policies": policies, "ckb_block_cycles": cycle_limits[0], "setup_height": setup_height,
                  "competing_pools": pools, "losing_pool_after_sync": losing_state,
                  "divergence": divergence, "sync_tip": sync_tip, "sync_ms": sync_ms,
                  "winner": {"transaction": competing_hash, "estimated_cycles": winner_cycles, "inclusion": inclusion},
                  "publisher": publisher.report(), "successor": confirmed, "before_pool": before_pool,
                  "after_pool": after_pool, "relayed": relayed, "own_funding": point(deployed, 2),
                  "before_resources": before_resources, "after_resources": after_resources}
        write_json(args.output / "ckb-checkpoint-contention-report.json", report)
        print(f"{len(members)}-member competing checkpoints reconciled, {len(relayed)} follow-up transactions relayed")
    finally:
        if reader.poll() is None:
            reader.terminate()
        try:
            reader.wait(timeout=8)
        except TimeoutExpired:
            reader.kill()
            reader.wait()

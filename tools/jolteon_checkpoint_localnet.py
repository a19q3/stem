"""Publish sparse Jolteon checkpoint proofs on a disposable CKB chain."""

import json
import subprocess
import time

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output, point, script, script_hash, transaction, wallet_group
from localnet import run, write_json
from native_bridge_localnet import wait_for
from stake_localnet import witness


class Relay:
    def __init__(self, args, l1, fixture, anchor, archive, spec, members, client, *, directory="jolteon-checkpoints"):
        self.args, self.l1, self.fixture, self.anchor, self.archive, self.client = args, l1, fixture, anchor, archive, client
        self.spec, self.members = spec, members
        self.root = args.output / directory
        self.root.mkdir()
        initial = le(0, 8) + le(1, 8) + b"".join(binary(member["ed25519"]) + le(member["power"], 8)
                                                   for member in members)
        genesis = binary(json.loads(run(args.binary, "genesis-hash", "--genesis", spec, "--header")))
        network = binary(anchor["network"])
        genesis_id = ckb_hash(b"Stem Header\0" + network + genesis)
        self.policy = hex_bytes(anchor["id"] + network + binary(anchor["genesis"]) + genesis_id
                                + ckb_hash(b"Stem Jolteon checkpoint roster\0" + initial))
        self.type = script(anchor["hash"], self.policy, "data2")
        self.lock = dict(self.type, args=hex_bytes(script_hash(self.type)))
        self.roster = self.root / "initial-roster.json"
        write_json(self.roster, hex_bytes(initial))
        self.previous = self.point = self.last_bundle = None
        self.funding = anchor["seed"]
        self.publications, self.rejections = [], {}

    def header(self, height):
        return bytes.fromhex(self.client.header(height)["header"])

    def bundle(self, height, *, proof_file=None, terminal=False):
        label = f"checkpoint-{height}"
        if proof_file is None:
            signed = self.client.finality(height)
            proof_file = self.root / f"{label}-proof.json"
            write_json(proof_file, signed["proof"])
            terminal = signed["handoff"]
        command = [self.args.binary, "checkpoint", "jolteon-bundle", "--archive-db", self.archive,
                   "--index", self.root / "history", "--network", self.anchor["network"], "--height", str(height),
                   "--policy", self.policy, "--roster", self.roster, "--proof", proof_file]
        if terminal:
            command.append("--terminal")
        if self.previous is not None:
            command += ["--previous-tip", self.previous]
        bundle = json.loads(run(*command))
        write_json(self.root / f"{label}-bundle.json", bundle)
        return bundle

    def advance(self, height, secp, *, proof_file=None, terminal=False, probe=False, submit=None):
        if self.publications and height == self.publications[-1]["height"]:
            return dependency(self.point)
        label = f"checkpoint-{height}"
        bundle = self.bundle(height, proof_file=proof_file, terminal=terminal)
        raw = binary(bundle["witness"])
        capacity = 1000 * CKB
        if self.point is not None:
            previous = self.l1.call("get_live_cell", self.point, False)["cell"]["output"]
            capacity = int(previous["capacity"], 16)

        def sign(name, tip, evidence):
            inputs = [self.point, self.funding] if self.point else [self.funding]
            payer = self.fixture.friend if self.point else self.fixture.faucet
            available = int(self.l1.call("get_live_cell", self.funding, True)["cell"]["output"]["capacity"], 16)
            change = available - FEE - (0 if self.point else capacity)
            tx = transaction(inputs, [output(capacity, self.lock, self.type), output(change, self.fixture.friend_lock)],
                             [tip, "0x"], [self.anchor["code"], secp])
            tx["witnesses"] = [witness(evidence)]
            return self.fixture.sign(f"{self.root.name}-{name}", tx, [wallet_group(len(inputs) - 1, [payer])])

        if probe and self.point is None:
            corrupt = bytearray(binary(bundle["tip"]))
            corrupt[-1] ^= 1
            self.rejections["different_execution_root"] = self.l1.reject(sign("bad-tip", hex_bytes(corrupt), raw))
        if probe and self.last_bundle is not None:
            previous = self.last_bundle
            self.rejections["replayed_handoff"] = self.l1.reject(sign("replay-handoff", previous["tip"], binary(previous["witness"])))
        signed = sign(label, bundle["tip"], raw)
        estimated = int(self.l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        tx = (submit or self.l1.send)(signed)
        self.point, self.funding = point(tx, 0), point(tx, 1)
        self.publications.append({"height": height, "transaction": tx, "estimated_cycles": estimated,
                                  "witness_bytes": len(raw), "l1_inclusion": self.l1.prove_committed(tx)})
        self.previous = self.root / f"{label}-tip.json"
        write_json(self.previous, bundle["tip"])
        self.roster = self.root / f"{label}-next-roster.json"
        write_json(self.roster, bundle["next_roster"])
        self.last_bundle = bundle
        return dependency(self.point)

    def foreign(self, anchor):
        return Relay(self.args, self.l1, self.fixture, anchor, self.archive, self.spec, self.members, self.client,
            directory="bridge-foreign-checkpoints")

    def audit_burn(self, label, height, header):
        # Exercise guardian authentication of the burn as an ancestor of a later finalized Header.
        finality = self.client.finality(height + 1)
        proof_file = self.root / f"{label}-raw-proof.json"
        write_json(proof_file, finality["proof"])
        command = [self.args.binary, "jolteon-proof", "--genesis", self.spec, "--proof", proof_file]
        prior = [self.client.handoff(epoch)["proof"] for epoch in range(finality["epoch"])]
        handoffs_file = None
        if prior:
            for epoch, handoff in enumerate(prior):
                path = self.root / f"authenticated-handoff-{epoch}.json"
                write_json(path, handoff)
                command += ["--handoff", path]
            handoffs_file = self.args.output / f"{label}-handoffs.json"
            write_json(handoffs_file, ["0x" + proof for proof in prior])
        if finality["handoff"]:
            command.append("--terminal")
        bundle = {"proof": hex_bytes(bytes.fromhex(finality["proof"])), "terminal": finality["handoff"]}
        checked = json.loads(run(*command))
        if checked["height"] != height:
            history = json.loads(run(self.args.binary, "checkpoint", "history", "--archive-db", self.archive,
                "--index", self.root / "history", "--network", self.anchor["network"],
                "--height", str(checked["height"]), "--header", str(height)))
            assert binary(history["header"]) == header
            bundle["history"] = {key: history[key] for key in ("frontier", "siblings")}
        else:
            assert bytes.fromhex(checked["header"]) == header
        path = self.args.output / f"{label}-finality.json"
        write_json(path, bundle)
        return path, [], checked, handoffs_file

    def report(self):
        return {"checkpoint_type": hex_bytes(script_hash(self.type)), "publications": self.publications,
                "rejections": self.rejections}


class PublisherFixture:
    """Drive the real publisher CLI with a fresh process on each poll."""

    def __init__(self, args, l1, fixture, relay, fee_dep, *, name="publisher"):
        self.args, self.l1, self.relay = args, l1, relay
        root = relay.root
        self.path = root / f"{name}.json"
        self.events_path = root / f"{name}-events.json"
        self.config = {
            "l1": l1.url, "replica": f"127.0.0.1:{relay.client.port}",
            "archive_db": str(relay.archive), "index": str(root / f"{name}-index"),
            "journal": str(root / ("journal" if name == "publisher" else f"{name}-journal")), "policy": relay.policy, "initial_roster": str(relay.roster),
            "checkpoint_code": relay.anchor["code"]["out_point"], "checkpoint_code_hash": relay.anchor["hash"],
            "seed": relay.anchor["seed"], "fee_dep_group": fee_dep["out_point"],
            "fee_key": str(fixture.faucet), "checkpoint_capacity": 1000 * CKB,
            "fee": FEE, "interval_seconds": 4 * 3600, "confirmations": 2,
        }
        self.events, self.publications = [], []

    def tick(self, label, requested=None, error=None, fee_fund=None, *, adopt=()):
        write_json(self.path, self.config)
        command = [self.args.binary, "checkpoint", "publish", "--config", self.path]
        if requested is not None:
            command += ["--at-least-height", str(requested)]
        if fee_fund is not None:
            command += ["--fee-fund", fee_fund]
        for update in adopt:
            command += ["--adopt", update]
        started = time.perf_counter()
        result = subprocess.run(command, text=True, capture_output=True, timeout=30)
        event = {"label": label, "command": list(map(str, command)), "configuration": dict(self.config),
                 "stdout": result.stdout, "stderr": result.stderr, "returncode": result.returncode,
                 "wall_seconds": time.perf_counter() - started}
        self.events.append(event)
        write_json(self.events_path, self.events)
        if error is not None:
            assert result.returncode and error in result.stderr, event
            return event
        assert result.returncode == 0, event
        return json.loads(result.stdout)

    def mine_inclusion(self, transaction):
        for _ in range(20):
            state = self.l1.call("get_transaction", transaction)["tx_status"]
            if state["status"] == "committed":
                return state
            assert state["status"] != "rejected", state
            self.l1.mine(1)
        raise AssertionError(f"checkpoint {transaction} was not mined")

    def confirm(self, label, submission):
        self.l1.mine(self.config["confirmations"])
        final = self.tick(label)
        assert final["status"] == "confirmed" and final["transaction"] == submission["transaction"], final
        final["proof"] = self.l1.prove_committed(final["transaction"])
        final["signed"] = self.l1.call("get_transaction", final["transaction"], "0x0", True)["transaction"]
        self.publications.append(final)
        return final

    def publish(self, label, height):
        submission = self.tick(label, height)
        assert submission["status"] == "submitted" and submission["stem_height"] >= height, submission
        self.mine_inclusion(submission["transaction"])
        return self.confirm(f"confirm-{label}", submission)

    def report(self):
        return {"checkpoint_type": hex_bytes(script_hash(self.relay.type)),
                "publications": self.publications, "events": self.events}


def exercise_publisher(args, l1, fixture, relay, fee_dep, agreed):
    publisher = PublisherFixture(args, l1, fixture, relay, fee_dep)
    config, tick = publisher.config, publisher.tick
    mine_inclusion, confirm = publisher.mine_inclusion, publisher.confirm
    first = tick("publish-first-handoff")
    assert (first["status"], first["stem_height"], first["next_epoch"]) == ("submitted", 10, 1), first
    first_tx = first["transaction"]
    restarted = tick("restart-before-inclusion")
    assert restarted["transaction"] == first_tx and restarted["status"] in ("in_pool", "submitted"), restarted
    original = mine_inclusion(first_tx)
    shallow = tick("restart-before-confirmations")
    assert shallow["status"] == "awaiting_confirmations" and shallow["depth"] == 0, shallow
    l1.call("truncate", l1.call("get_block_hash", hex(int(original["block_number"], 16) - 1)))
    recovered = tick("resume-after-unconfirmed-reorg")
    assert recovered["transaction"] == first_tx and recovered["status"] in ("in_pool", "submitted"), recovered
    included = mine_inclusion(first_tx)
    assert included["block_hash"] != original["block_hash"], (original, included)
    confirm("confirm-first-handoff", first)

    second = tick("publish-second-handoff")
    assert (second["status"], second["stem_height"], second["next_epoch"]) == ("submitted", 20, 2), second
    mine_inclusion(second["transaction"])
    confirm("confirm-second-handoff", second)

    idle = tick("respect-regular-interval")
    assert idle["status"] == "idle" and idle["stem_height"] == 20 and idle["latest_finalized"] >= agreed, idle
    future = tick("request-unfinalized-height", idle["latest_finalized"] + 1000)
    assert future["status"] == "idle", future
    demand = tick("publish-on-demand", agreed)
    assert demand["status"] == "submitted" and demand["stem_height"] == agreed, demand
    mine_inclusion(demand["transaction"])
    confirm("confirm-on-demand", demand)
    repeat = tick("repeat-satisfied-request", agreed)
    assert repeat["status"] == "idle" and repeat["stem_height"] == demand["stem_height"], repeat

    config["interval_seconds"] = 1
    previous = int.from_bytes(relay.header(demand["stem_height"])[8:16], "little")
    for _ in range(40):
        height = relay.client.status()["height"]
        if int.from_bytes(relay.header(height)[8:16], "little") >= previous + 1000:
            break
        time.sleep(0.1)
    else:
        raise AssertionError("Stem finalized time did not advance by the regular publication interval")
    routine = tick("publish-by-interval")
    assert routine["status"] == "submitted" and routine["stem_height"] > demand["stem_height"], routine
    mine_inclusion(routine["transaction"])
    anchor = confirm("confirm-regular", routine)

    config["confirmations"] = 1
    tick("reject-different-journal-configuration", error="journal belongs to a different checkpoint")
    config["confirmations"] = 2
    height = anchor["l1_height"]
    l1.call("truncate", l1.call("get_block_hash", hex(height - 1)))
    l1.mine(1)
    assert l1.call("get_block_hash", hex(height)) != anchor["l1_block"]
    halted = tick("halt-after-confirmed-reorg", error="confirmed checkpoint anchor was reorganized")
    return {**publisher.report(), "first_inclusion": original, "replacement_inclusion": included,
            "rejected_confirmed_reorg": halted}


def exercise_publisher_handoff(args, l1, fixture, relay, scenario, agreed):
    a = PublisherFixture(args, l1, fixture, relay, scenario.secp, name="publisher-a")
    b = PublisherFixture(args, l1, fixture, relay, scenario.secp, name="publisher-b")
    late = PublisherFixture(args, l1, fixture, relay, scenario.secp, name="late-publisher")
    b.config.update(fee_key=str(fixture.friend), update_funding=scenario.relay_fee_point)
    late.config.update(fee_key=str(fixture.friend), update_funding=scenario.relay_fee_point)

    auto = args.auto_follow
    if auto:
        for publisher in (a, b, late):
            publisher.config["auto_follow"] = True

    def indexed():
        expected = l1.call("get_tip_block_number")
        tip = wait_for(lambda: l1.call("get_indexer_tip"), "CKB Indexer initialization", seconds=15)
        if tip["block_number"] != expected:
            wait_for(lambda: l1.call("get_indexer_tip")["block_number"] == expected,
                     "CKB Indexer catching up", seconds=15)

    def awaiting(publisher, label, pinned):
        step = publisher.tick(label)
        assert step["status"] == "awaiting_successor" and step["checkpoint"] == pinned, step

    def adopted(publisher, label, hashes, height, epoch, count=1, *, explicit=False):
        if auto and not explicit:
            indexed()
        step = publisher.tick(label, adopt=hashes if explicit or not auto else ())
        assert (step["status"], step["updates"], step["transaction"], step["stem_height"], step["next_epoch"]) == (
            "adopted", count, hashes[-1], height, epoch), step
        return step

    late.tick("reject-unrelated-transaction", error="lacks the pinned checkpoint Type Script",
              adopt=[scenario.relay_fee_point["tx_hash"]])
    first = a.tick("a-bootstraps")
    assert (first["status"], first["stem_height"], first["next_epoch"]) == ("submitted", 10, 1), first
    first_hash = first["transaction"]
    a.mine_inclusion(first_hash)
    if auto:
        awaiting(b, "wait-for-bootstrap-confirmations", relay.anchor["seed"])
    b.tick("reject-before-confirmations", error="not sufficiently confirmed", adopt=[first_hash])
    a.confirm("a-confirms-bootstrap", first)
    adopted(b, "b-adopts-bootstrap", [first_hash], 10, 1)
    foreign = relay.foreign(scenario.foreign_checkpoint)
    foreign.advance(10, scenario.secp)
    l1.mine(late.config["confirmations"])
    late.tick("reject-valid-foreign-checkpoint", error="lacks the pinned checkpoint Type Script",
              adopt=[foreign.point["tx_hash"]])

    second = a.tick("a-submits-committee-handoff")
    assert (second["status"], second["stem_height"], second["next_epoch"]) == ("submitted", 20, 2), second
    second_hash = second["transaction"]
    b.tick("b-competes-for-same-checkpoint", 20, error="L1 send_transaction failed")
    a.mine_inclusion(second_hash)
    if auto:
        awaiting(b, "wait-for-winning-spend-confirmations", point(first_hash, 0))
    a.confirm("a-confirms-handoff", second)
    late.tick("reject-missing-bootstrap", error="does not bootstrap from the pinned seed", adopt=[second_hash])
    b.tick("reject-unknown-tail-without-saving-preceding-update", error="L1 transaction is unconfirmed",
           adopt=[second_hash, "0x" + "42" * 32])
    adopted(b, "b-adopts-winner-after-conflict", [first_hash, second_hash], 20, 2)
    adopted(b, "b-repeats-last-adoption", [second_hash], 20, 2, count=0, explicit=True)

    height = max(21, agreed)
    update = b.tick("b-publishes-with-independent-wallet", height)
    assert update["status"] == "submitted" and update["stem_height"] == height, update
    update_hash = update["transaction"]
    b.mine_inclusion(update_hash)
    if auto:
        local = b.tick("b-retains-own-shallow-confirmation", height)
        assert local["status"] == "awaiting_confirmations" and local["transaction"] == update_hash, local
        b.confirm("b-retains-own-funding-confirmation", update)
    else:
        l1.mine(b.config["confirmations"])
        adopted(b, "b-adopts-own-pending-winner", [update_hash], height, 2)
        b.publications.append(dict(update, proof=l1.prove_committed(update_hash),
            signed=l1.call("get_transaction", update_hash, "0x0", True)["transaction"]))
    inputs = l1.call("get_transaction", update_hash, "0x2", True)["transaction"]["inputs"]
    assert [i["previous_output"] for i in inputs] == [point(second_hash, 0), scenario.relay_fee_point], inputs
    adopted(a, "a-adopts-b", [update_hash], height, 2)
    late.tick("reject-skipped-handoff", error="does not spend the preceding adopted OutPoint",
              adopt=[first_hash, update_hash])

    def finalized(height):
        for _ in range(80):
            if relay.client.status()["height"] >= height:
                return
            time.sleep(0.1)
        raise TimeoutError(f"Stem did not finalize {height}")

    # Rebuild the proofs used here from the actual Stem archive. The manual
    # successor demonstrates that adoption follows real CKB group positions.
    for h in (10, 20, height):
        bundle = relay.bundle(h)
        relay.previous = relay.root / f"manual-prior-{h}.json"
        relay.roster = relay.root / f"manual-roster-{h}.json"
        write_json(relay.previous, bundle["tip"])
        write_json(relay.roster, bundle["next_roster"])
    target = height + 1
    finalized(target)
    shifted = relay.bundle(target)
    fee_input = point(second_hash, 1)
    balance = int(l1.call("get_live_cell", fee_input, True)["cell"]["output"]["capacity"], 16)
    old_capacity = int(l1.call("get_live_cell", point(update_hash, 0), True)["cell"]["output"]["capacity"], 16)
    tx = transaction([fee_input, point(update_hash, 0)],
         [output(balance - FEE - CKB, fixture.standard), output(old_capacity + CKB, relay.lock, relay.type)],
         ["0x", shifted["tip"]], [relay.anchor["code"], scenario.secp])
    tx["witnesses"] = ["0x", witness(binary(shifted["witness"]))]
    shifted_signed = fixture.sign("shifted-checkpoint", tx, [wallet_group(0, [fixture.faucet])])
    same_block = None
    if auto:
        moved = l1.call("send_transaction", shifted_signed["json"], "passthrough")
        assert moved == shifted_signed["hash"]
        finalized(target + 1)
        relay.previous = relay.root / "same-block-parent-tip.json"
        relay.roster = relay.root / "same-block-parent-roster.json"
        write_json(relay.previous, shifted["tip"])
        write_json(relay.roster, shifted["next_roster"])
        next_bundle = relay.bundle(target + 1)
        child = transaction([point(moved, 1), point(moved, 0)],
            [output(old_capacity + CKB, relay.lock, relay.type), output(balance - 2 * FEE - CKB, fixture.standard)],
            [next_bundle["tip"], "0x"], [relay.anchor["code"], scenario.secp])
        child["witnesses"] = [witness(binary(next_bundle["witness"]))]
        child_signed = fixture.sign("same-block-checkpoint", child, [wallet_group(1, [fixture.faucet])])
        tail = l1.send(child_signed)
        tail_proof, moved_proof = l1.prove_committed(tail), l1.prove_committed(moved)
        assert tail_proof["header"] == moved_proof["header"], (moved_proof, tail_proof)
        same_block = {"transaction": tail, "proof": tail_proof, "signed": child_signed}
        l1.mine(b.config["confirmations"])
        adopted(b, "b-pins-first-position-in-shared-ckb-block", [moved], target, 2, explicit=True)
        shifted_head = adopted(b, "b-discovers-second-position-in-same-block", [tail], target + 1, 2)
        tail_point = point(tail, 0)
        assert shifted_head["checkpoint"] == tail_point
        all_updates = adopted(late, "late-discovers-all-committees", [first_hash, second_hash, update_hash, moved, tail],
                              target + 1, 2, count=5)
        target += 1
    else:
        moved = l1.send(shifted_signed)
        moved_proof = l1.prove_committed(moved)
        l1.mine(b.config["confirmations"])
        adopted(b, "b-adopts-output-and-group-input-one", [moved], target, 2)
        all_updates = adopted(late, "late-adopts-all-committees", [first_hash, second_hash, update_hash, moved],
                              target, 2, count=4)
        tail_point = point(moved, 1)
    assert all_updates["checkpoint"] == tail_point, all_updates
    finalized(target + 1)
    successor = b.publish("b-resumes-from-shifted-output", target + 1)
    successor_hash = successor["transaction"]
    resumed = l1.call("get_transaction", successor_hash, "0x2", True)["transaction"]
    assert [i["previous_output"] for i in resumed["inputs"]] == [tail_point, point(update_hash, 1)], resumed
    assert int(resumed["outputs"][0]["capacity"], 16) == old_capacity + CKB, resumed

    pages = None
    anchor = successor
    if args.auto_follow_long:
        pages = exercise_follow_pages(args, l1, fixture, relay, scenario, b, successor, finalized, indexed, adopted)
        anchor = pages["resumed"]
    inclusion = anchor["l1_height"]
    l1.call("truncate", l1.call("get_block_hash", hex(inclusion - 1)))
    l1.mine(1)
    assert l1.call("get_block_hash", hex(inclusion)) != anchor["l1_block"]
    b.tick("halt-after-confirmed-publisher-reorg", error="confirmed checkpoint anchor was reorganized")
    b.tick("refuse-adoption-after-confirmed-reorg", error="confirmed checkpoint anchor was reorganized",
           adopt=[moved])
    return {"a": a.report(), "b": b.report(), "late": late.report(), "foreign": foreign.report(),
            "shifted": {"transaction": moved, "proof": moved_proof, "signed": shifted_signed},
            "same_block": same_block, "auto_follow": auto, "paging": pages,
            "confirmed_reorg": {"orphaned": anchor["transaction"], "replacement": l1.call("get_block_hash", hex(inclusion))}}


def exercise_follow_pages(args, l1, fixture, relay, scenario, publisher, previous, finalized, indexed, adopted):
    funding = scenario.deployment_change
    balance = int(l1.call("get_live_cell", funding, True)["cell"]["output"]["capacity"], 16)
    tx = transaction([funding],
        [output(1000 * CKB, fixture.friend_lock), output(balance - 1000 * CKB - FEE, fixture.standard)],
        ["0x", "0x"], [scenario.secp])
    wallet = l1.send(fixture.sign("paged-checkpoints-fee-source", tx, [wallet_group(0, [fixture.faucet])]))
    remote = Relay(args, l1, fixture, relay.anchor, relay.archive, relay.spec, relay.members, relay.client,
                   directory="paged-checkpoints")
    remote.point, remote.funding = point(previous["transaction"], 0), point(wallet, 0)
    committed = l1.call("get_transaction", previous["transaction"], "0x2", True)["transaction"]
    remote.previous = remote.root / "previous-tip.json"
    write_json(remote.previous, committed["outputs_data"][0])
    remote.roster = relay.roster
    start = previous["stem_height"]
    for height in range(start + 1, start + 66):
        finalized(height)
        remote.advance(height, scenario.secp)
        write_json(remote.root / "publication-progress.json", remote.publications)
    l1.mine(publisher.config["confirmations"])
    indexed()
    txs = [entry["transaction"] for entry in remote.publications]
    first = adopted(publisher, "follow-first-64-checkpoints", txs[:64], start + 64, 2, count=64)
    second = adopted(publisher, "follow-remaining-checkpoint", txs[64:], start + 65, 2)
    finalized(start + 66)
    resumed = publisher.publish("publish-after-paged-recovery", start + 66)
    committed = l1.call("get_transaction", resumed["transaction"], "0x2", True)["transaction"]
    assert [input_["previous_output"] for input_ in committed["inputs"]] == [
        point(second["transaction"], 0), point(previous["transaction"], 1),
    ], committed
    return {"remote_fee_input": point(wallet, 0), "remote": remote.report(), "first": first, "second": second,
            "resumed": resumed}

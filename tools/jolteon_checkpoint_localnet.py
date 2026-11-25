"""Publish sparse Jolteon checkpoint proofs on a disposable CKB chain."""

import json

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output, point, script, script_hash, transaction, wallet_group
from localnet import run, write_json
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

    def advance(self, height, secp, *, proof_file=None, terminal=False, probe=False):
        if self.publications and height == self.publications[-1]["height"]:
            return dependency(self.point)
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
        raw = binary(bundle["witness"])

        def sign(name, tip, evidence):
            inputs = [self.point, self.funding] if self.point else [self.funding]
            capacity = 1000 * CKB
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
        tx = self.l1.send(signed)
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
        return path, [], checked

    def report(self):
        return {"checkpoint_type": hex_bytes(script_hash(self.type)), "publications": self.publications,
                "rejections": self.rejections}

#!/usr/bin/env python3
"""Run independent service bonds, payouts, penalties and closure on CKB devnet."""

import argparse
import copy
import json
from pathlib import Path
import secrets
import subprocess
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from bridge_localnet import (
    CKB, FEE, FAUCET_ARG, Fixtures, L1, binary, ckb_hash, dependency, hex_bytes, l1_reference_request, le,
    output, point, script, script_hash, transaction, wallet_group, write_key,
)
from localnet import run, write_json
from stake_localnet import witness


class Scenario:
    def __init__(self, args, l1, fixture, contract, *, funding=None, network=None):
        self.args, self.l1, self.fixture = args, l1, fixture
        self.rejections, self.committed, self.transactions = {}, {}, {}
        self.release_lock = fixture.standard
        self.buyer = fixture.owner_lock
        self.penalty = script(fixture.standard["code_hash"], fixture.key(fixture.l1_keys[2])["blake160"])
        self.challenge = 12
        self.receipt_capacity = 194 * CKB
        block = l1.call("get_block_by_number", "0x0")
        self.genesis = block["header"]["hash"]
        faucet, system = block["transactions"][:2]
        index, source = next((i, cell) for i, cell in enumerate(faucet["outputs"]) if cell["lock"]["args"] == FAUCET_ARG)
        self.secp = dependency(point(system["hash"], 0), "dep_group")
        self.wallet = funding or point(faucet["hash"], index)
        if funding:
            source = l1.call("get_live_cell", funding, True)["cell"]["output"]
        assert source["lock"] == self.release_lock
        self.wallet_capacity = int(source["capacity"], 16)
        code = contract.read_bytes()
        self.code_hash = hex_bytes(ckb_hash(code))
        self.network = network or hex_bytes(ckb_hash(b"Stem security domain fixture\0" + binary(self.genesis)))
        self.members = []
        for beneficiary in (fixture.owner, fixture.friend, *fixture.l1_keys[:2]):
            key = Ed25519PrivateKey.from_private_bytes(secrets.token_bytes(32))
            public = key.public_key().public_bytes_raw()
            payee = script(fixture.standard["code_hash"], fixture.key(beneficiary)["blake160"])
            self.members.append((public, key, payee))
        self.members.sort(key=lambda item: item[0])
        self.bonds = [x * CKB for x in (300, 220, 180, 100)]
        self.member_files = [write_key(args.output, f"service-{i}", key.private_bytes_raw())
                             for i, (_, key, _) in enumerate(self.members)]
        self.deploy(code)

    def pay_transaction(self, inputs, outputs, data, deps, *, change_delta=0):
        change = self.wallet_capacity + change_delta - FEE
        return transaction(inputs + [self.wallet], outputs + [output(change, self.release_lock)],
                           data + ["0x"], deps + [self.secp])

    def signed(self, name, tx, additional=()):
        fee = wallet_group(len(tx["inputs"]) - 1, [self.fixture.faucet])
        return self.fixture.sign("security-" + name, tx, [*additional, fee])

    def accept(self, name, tx, *, additional=()):
        signed = self.signed(name, tx, additional)
        cycles = int(self.l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        committed = self.l1.send(signed)
        self.wallet = point(committed, len(tx["outputs"]) - 1)
        self.wallet_capacity = int(tx["outputs"][-1]["capacity"], 16)
        self.committed[name] = {"hash": committed, "cycles": cycles, "bytes": len(binary(signed["molecule"])),
                                "inclusion": self.l1.prove_committed(committed)}
        self.transactions[name] = signed["json"]
        write_json(self.args.output / "security-measurements.json", self.committed)
        return committed

    def reject(self, name, tx, error, *, signed=None):
        signed = signed or self.signed(name, tx)
        try:
            self.l1.call("send_transaction", signed["json"], "passthrough")
        except RuntimeError as exc:
            assert error in str(exc), (name, error, str(exc))
            self.rejections[name] = str(exc)
            write_json(self.args.output / "security-rejections.json", self.rejections)
            return
        raise AssertionError(f"CKB accepted invalid security transition {name}")

    def deploy(self, code):
        self.subject_data = b"independent CKB application state"
        deployed = self.accept("deploy", self.pay_transaction(
            [], [output((len(code) + 100) * CKB, self.release_lock),
                 output(120 * CKB, self.release_lock), output(120 * CKB, self.release_lock),
                 output(100 * CKB, self.penalty)],
            [hex_bytes(code), hex_bytes(self.subject_data), hex_bytes(b"other Cell"), "0x"], [],
            change_delta=-(len(code) + 440) * CKB))
        self.code = dependency(point(deployed, 0))
        self.subject = point(deployed, 1)
        self.foreign_subject = point(deployed, 2)
        self.foreign_operator = point(deployed, 3)

    def open_domain(self):
        draft = self.args.output / "security-domain-request.json"
        write_json(draft, {"code_hash": self.code_hash, "seed": self.wallet, "output_index": 0,
            "ckb_genesis": self.genesis, "network": self.network, "operator": self.release_lock["args"],
            "penalty": self.penalty["args"], "quorum_bps": 7500, "challenge_blocks": self.challenge,
            "members": [{"public_key": hex_bytes(public), "beneficiary": payee["args"], "bond": bond}
                        for (public, _, payee), bond in zip(self.members, self.bonds)]})
        self.domain = json.loads(run(self.args.binary, "security-domain", "domain", "--request", draft))
        write_json(self.args.output / "security-domain.json", self.domain)
        self.state = binary(self.domain["state"])
        self.capacity = self.domain["capacity"]
        assert self.capacity - self.domain["occupied_shannons"] == sum(self.bonds)
        tx = self.pay_transaction([], [self.domain_output()], [self.domain["state"]], [self.code],
                                  change_delta=-self.capacity)
        tx["header_deps"] = [self.genesis]
        underfunded = copy.deepcopy(tx)
        underfunded["outputs"][0]["capacity"] = hex(self.capacity - 1)
        self.reject("underfunded-domain", underfunded, "error code 97")
        self.head = point(self.accept("open-domain", tx), 0)
        self.initial_head = self.head

    def domain_output(self, capacity=None):
        return output(self.capacity if capacity is None else capacity, self.domain["lock"], self.domain["type"])

    def reject_capital_reuse(self):
        root = self.args.output
        original = json.loads((root / "security-domain-request.json").read_text())
        original["seed"] = self.head
        draft = root / "security-reuse-domain-request.json"
        write_json(draft, original)
        another = json.loads(run(self.args.binary, "security-domain", "domain", "--request", draft))
        assert another["domain"] != self.domain["domain"]
        tx = self.pay_transaction([self.head],
            [output(self.capacity, another["lock"], another["type"])], [another["state"]], [self.code])
        tx["header_deps"] = [self.genesis]
        tx["witnesses"] = [witness(b"\x03")]
        self.reject("reuse-active-bond-in-new-domain", tx, "error code 99")

    def open_request(self, name, buyer=None, domain=None):
        terms = self.args.output / f"security-{name}-terms.json"
        buyer = buyer or self.buyer
        refund = int(self.l1.call("get_tip_block_number"), 16) + 14
        write_json(terms, {"code_hash": self.code_hash, "domain": domain or self.domain["domain"],
                           "buyer": buyer["args"], "subject": self.subject,
                           "refund_height": refund, "receipt_capacity": self.receipt_capacity})
        lock = json.loads(run(self.args.binary, "security-domain", "request", "--request", terms))
        reward = 600 * CKB
        amount = reward + self.receipt_capacity
        tx = self.pay_transaction([], [output(amount, lock)], ["0x"], [], change_delta=-amount)
        result = self.accept(name, tx)
        return {"point": point(result, 0), "capacity": amount, "reward": reward, "buyer": buyer, "refund": refund}

    def sign_command(self, request, index):
        domain = self.args.output / "security-signer-domain-point.json"
        request_path = self.args.output / "security-signer-request-point.json"
        write_json(domain, self.head)
        write_json(request_path, request["point"])
        return [self.args.binary, "security-domain", "sign", "--domain", domain, "--request", request_path,
                "--code-hash", self.code_hash, "--l1-rpc", self.l1.url, "--key-file", self.member_files[index],
                "--journal", self.args.output / f"signer-{index}-journal"]

    def collect(self, name, request, indices):
        shares = [json.loads(run(*self.sign_command(request, i))) for i in indices]
        assert [share["index"] for share in shares] == list(indices)
        assert len({share["result"] for share in shares}) == 1
        assert shares[0] == json.loads(run(*self.sign_command(request, indices[0])))
        write_json(self.args.output / f"security-{name}-votes.json", shares)
        return shares

    def receipt_type(self, request):
        return script(self.code_hash, hex_bytes(binary(self.domain["domain"]) + outpoint(request["point"])), "data2")

    def service(self, request, shares, *, deps=None, result=None):
        result = result or shares[0]["result"]
        indices = [share["index"] for share in shares]
        selected = sum(self.bonds[i] for i in indices)
        payouts = [request["reward"] * self.bonds[i] // selected for i in indices]
        payouts[0] += request["reward"] - sum(payouts)
        certificate = binary(result) + bytes([sum(1 << i for i in indices)]) + b"".join(
            binary(share["signature"]) for share in shares)
        outputs = [self.domain_output()] + [output(a, self.members[i][2]) for i, a in zip(indices, payouts)]
        outputs.append(output(self.receipt_capacity, request["buyer"], self.receipt_type(request)))
        tx = self.pay_transaction([self.head, request["point"]], outputs,
                                  [hex_bytes(self.state)] + ["0x"] * len(shares) + [result],
                                  deps or [dependency(self.subject), self.code])
        tx["witnesses"] = [witness(b"\0" + certificate)]
        return tx

    def verify_receipt(self, tx, index):
        evidence = self.l1.prove_committed(tx)
        packed = self.l1.call("get_transaction", tx, "0x0", True)["transaction"]
        transaction_file = self.args.output / "security-receipt-packed.json"
        proof_file = self.args.output / "security-receipt-proof.json"
        write_json(transaction_file, packed)
        write_json(proof_file, evidence)
        observed = json.loads(run(self.args.binary, "ckb-wallet", "verify-inclusion",
            "--transaction", transaction_file, "--proof", proof_file, "--block-hash", evidence["header"]["hash"],
            "--tx-hash", tx, "--output-index", str(index)))
        return observed

    def conflict(self, request, share):
        index = share["index"]
        correct = binary(share["result"])
        alternate = ckb_hash(b"Conflicting test report" + correct)
        first, second = sorted((correct, alternate))
        key = self.members[index][1]

        def record(result):
            return result + key.sign(self.report_digest(request, result))

        return bytes([index]) + outpoint(request["point"]) + record(first) + record(second)

    def report_digest(self, request, result):
        return ckb_hash(b"Stem CKB security domain live Cell attestation\0"
                        + binary(self.domain["domain"]) + outpoint(request["point"]) + result)

    def slash(self, index, proof):
        state = bytearray(self.state)
        state[2 + 61 * index + 60] = 1
        balance = self.capacity - self.bonds[index]
        tx = self.pay_transaction([self.head],
             [self.domain_output(balance), output(self.bonds[index], self.penalty)],
             [hex_bytes(state), "0x"], [self.code])
        tx["witnesses"] = [witness(b"\x02" + proof)]
        return tx, bytes(state), balance

    def retire(self):
        state = b"\x01" + self.state[1:]
        tx = self.pay_transaction([self.head], [self.domain_output()], [hex_bytes(state)], [self.code])
        tx["witnesses"] = [witness(b"\x01")]
        unauthorized = copy.deepcopy(tx)
        unauthorized["inputs"][1]["previous_output"] = self.foreign_operator
        unauthorized["outputs"][-1] = output(100 * CKB - FEE, self.penalty)
        invalid = self.fixture.sign("security-unauthorized-retire", unauthorized,
                                    [wallet_group(1, [self.fixture.l1_keys[2]])])
        self.reject("unauthorized-retire", unauthorized, "error code 99", signed=invalid)
        self.head = point(self.accept("retire", tx), 0)
        self.state = state

    def close(self, live):
        outputs = [output(self.bonds[i], self.members[i][2]) for i in live]
        reserve = self.capacity - sum(self.bonds[i] for i in live)
        assert reserve == self.domain["occupied_shannons"]
        outputs.append(output(reserve, self.release_lock))
        tx = self.pay_transaction([self.head], outputs, ["0x"] * len(outputs), [self.code])
        tx["inputs"][0]["since"] = hex(1 << 63 | self.challenge)
        tx["witnesses"] = [witness(b"\x03")]
        return tx

    def audit_scope(self, through):
        return {"ckb_genesis": self.genesis, "network": self.network, "code_hash": self.code_hash,
                "domain": self.domain["domain"], "through": through}

    def audit(self, label, through, *, live=False):
        base = self.args.output / f"security-audit-{label}"
        scope, proof, anchors = [base.with_suffix(f".{kind}.json") for kind in ("scope", "proof", "anchors")]
        request = self.audit_scope(through)
        write_json(scope, request)
        command = [self.args.binary, "security-domain", "audit", "--l1-rpc", self.l1.url,
                   "--scope", scope, "--confirmations", "2", "--proof-output", proof]
        if live:
            command.append("--require-live")
        started = time.monotonic()
        report = json.loads(run(*command))
        collection_seconds = time.monotonic() - started
        history = json.loads(proof.read_text())
        heights = {0, *(int(entry["header"]["number"], 16) for entry in history)}
        hashes = {height: self.l1.call("get_block_hash", hex(height)) for height in sorted(heights)}
        write_json(anchors, hashes)
        verifier = [self.args.binary, "security-domain", "verify-audit", "--scope", scope,
                    "--proof", proof, "--anchors", anchors]
        started = time.monotonic()
        assert report == json.loads(run(*verifier))
        return {"report": report, "scope": request, "history": history, "trusted_blocks": hashes,
                "proof_bytes": proof.stat().st_size, "collection_seconds": collection_seconds,
                "verification_seconds": time.monotonic() - started,
                "collect_command": [str(part) for part in command], "verify_command": [str(part) for part in verifier]}

    def verify_audits(self, active, *, reorg):
        def target(name, *, closure=False):
            tx = self.committed[name]["hash"]
            return {"closure": tx} if closure else {"cell": point(tx, 0)}

        def amount(record, name):
            return int(record[name], 16)

        reports = {"active-live": active}
        for label, name in (("open", "open-domain"), ("first-service", "serve-one"), ("slashed", "slash-one"),
                            ("retired", "retire"), ("challenge", "slash-during-challenge"), ("closed", "close")):
            reports[label] = self.audit(label, target(name, closure=label == "closed"))
        served = ("serve-one", "serve-two")
        signers = ((0, 1, 2), (0, 1, 3))
        expected = {
            "open": ("active", (), (), 0), "first-service": ("active", (), (), 1),
            "slashed": ("active", (2,), (), 1), "active-live": ("active", (2,), (), 2),
            "retired": ("retired", (2,), (), 2), "challenge": ("retired", (2, 3), (), 2),
            "closed": ("closed", (2, 3), (0, 1), 2),
        }
        for label, entry in reports.items():
            report = entry["report"]
            state, slashed, returned, services = expected[label]
            income = [0] * len(self.members)
            for tx, members in zip(served[:services], signers[:services]):
                for index, member in enumerate(members, 1):
                    income[member] += int(self.transactions[tx]["outputs"][index]["capacity"], 16)
            assert report["state"] == state and report["paid_services"] == services
            assert amount(report, "service_income") == sum(income)
            for i, member in enumerate(report["members"]):
                assert member["public_key"] == hex_bytes(self.members[i][0])
                assert member["beneficiary"] == self.members[i][2]["args"]
                assert amount(member, "initial_bond") == self.bonds[i]
                assert amount(member, "slashed_bond") == (self.bonds[i] if i in slashed else 0)
                assert amount(member, "returned_bond") == (self.bonds[i] if i in returned else 0)
                assert amount(member, "locked_bond") == (0 if i in slashed or i in returned else self.bonds[i])
                assert amount(member, "service_income") == income[i]
            assert amount(report, "slashed_bond") == sum(self.bonds[i] for i in slashed)
            assert amount(report, "returned_bond") == sum(self.bonds[i] for i in returned)
            locked = sum(self.bonds) - amount(report, "slashed_bond") - amount(report, "returned_bond")
            assert amount(report, "locked_bond") == locked
            eligible = locked if state == "active" else 0
            assert amount(report, "eligible_bond") == eligible
            quorum = sum(self.bonds) * 7500 // 10_000
            assert amount(report, "required_quorum_bond") == quorum
            assert report["can_attest"] == (eligible >= quorum)
            assert amount(report, "capacity") == (locked + self.domain["occupied_shannons"] if state != "closed" else 0)

        closed = reports["closed"]
        rejects = {}

        def reject(label, reason, *, scope=None, history=None, anchors=None, collect=None):
            if collect is None:
                command = closed["verify_command"].copy()
            else:
                online = active["collect_command"]
                command = online[:online.index("--proof-output")]
                for flag, value in collect.items():
                    if flag in command:
                        command[command.index(flag) + 1] = value
                    else:
                        command.append(flag)
                        if value is not None:
                            command.append(value)
            for name, data in (("scope", scope), ("proof", history), ("anchors", anchors)):
                if data is not None:
                    path = self.args.output / f"security-audit-rejected-{label}.{name}.json"
                    write_json(path, data)
                    command[command.index("--" + name) + 1] = str(path)
            attempt = subprocess.run(command, text=True, capture_output=True, timeout=30)
            assert attempt.returncode and reason in attempt.stderr, (label, attempt)
            rejects[label] = {"stderr": attempt.stderr, "command": command}

        reject("omitted-service", "missing a predecessor", history=closed["history"][:1] + closed["history"][2:])
        reject("truncated", "wrong transition", history=closed["history"][:-1])
        tampered = copy.deepcopy(closed["history"])
        encoded = binary(tampered[1]["transaction"])
        tampered[1]["transaction"] = hex_bytes(encoded[:-1] + bytes([encoded[-1] ^ 1]))
        reject("changed-witness", "transaction or witness proof", history=tampered)
        reject("foreign-program", "scope mismatch", scope=dict(closed["scope"], code_hash=self.network))
        reject("foreign-genesis", "wrong trusted CKB genesis", anchors={**closed["trusted_blocks"], 0: self.network})
        reject("historical-as-live", "no longer live", collect={"--require-live": None})
        reject("confirmation-depth", "required confirmations", collect={"--confirmations": "1000"})
        reject("history-budget", "transaction budget exhausted", collect={"--max-transactions": "1"})
        export = active["collect_command"]
        existing = Path(export[export.index("--proof-output") + 1])
        before = existing.read_bytes()
        reject("existing-evidence", "publish new security audit proof", collect={"--proof-output": str(existing)})
        assert existing.read_bytes() == before

        orphaned = None
        if reorg:
            header = self.committed["close"]["inclusion"]["header"]
            self.l1.call("truncate", header["parent_hash"])
            self.l1.mine(5)
            tip = int(self.l1.call("get_tip_block_number"), 16)
            replacement = {height: self.l1.call("get_block_hash", hex(height))
                           for height in closed["trusted_blocks"] if height <= tip}
            assert replacement[int(header["number"], 16)] != header["hash"]
            reject("orphaned-closure", "outside the trusted block", anchors=replacement)
            orphaned = {"removed_header": header, "replacement_anchors": replacement}
        return {"states": reports, "rejections": rejects, "reorganization": orphaned}

    def run(self, *, audit_reorg=False):
        self.open_domain()
        self.reject_capital_reuse()
        r1 = self.open_request("query-one")
        r2 = self.open_request("query-two")
        foreign = self.open_request("foreign-query", domain=hex_bytes(ckb_hash(b"foreign security domain")))
        attempt = subprocess.run(self.sign_command(foreign, 0), capture_output=True, text=True)
        assert attempt.returncode and "different domain" in attempt.stderr, attempt
        self.rejections["foreign-request-signing"] = attempt.stderr

        shares = self.collect("initial", r1, (0, 1, 2))
        duplicate = self.service(r2, shares)
        self.reject("same-signatures-another-request", duplicate, "error code 98")
        self.reject("insufficient-quorum", self.service(r1, shares[:2]), "error code 98")
        self.reject("substituted-subject", self.service(r1, shares,
                    deps=[dependency(self.foreign_subject), self.code]), "error code 98")
        false_value = bytes([1]) * 32
        false_votes = [dict(share, result=hex_bytes(false_value),
                            signature=hex_bytes(self.members[share["index"]][1].sign(
                                self.report_digest(r1, false_value)))) for share in shares]
        write_json(self.args.output / "security-false-value-votes.json", false_votes)
        self.reject("quorum-signed-false-subject-value", self.service(r1, false_votes), "error code 98")
        stolen = self.service(r1, shares)
        stolen["outputs"][1]["lock"] = self.release_lock
        self.reject("redirected-service-fee", stolen, "error code 97")
        counter = self.pay_transaction([], [output(self.receipt_capacity, self.buyer, self.receipt_type(r1))],
                                       [shares[0]["result"]], [self.code], change_delta=-self.receipt_capacity)
        self.reject("counterfeit-receipt", counter, "error code 94")
        delivered = self.accept("serve-one", self.service(r1, shares))
        self.head = point(delivered, 0)
        receipt = self.verify_receipt(delivered, 4)
        assert receipt["data"] == shares[0]["result"] and receipt["output"]["type"] == self.receipt_type(r1)
        plain = self.pay_transaction([receipt["out_point"]], [output(self.receipt_capacity, self.buyer)],
                                     ["0x"], [self.code])
        carried = copy.deepcopy(plain)
        carried["outputs"][0] = output(self.receipt_capacity, self.buyer, self.receipt_type(r1))
        carried["outputs_data"][0] = receipt["data"]
        receipt_owner = [wallet_group(0, [self.fixture.owner])]
        self.reject("reissued-receipt", carried, "error code 96",
                    signed=self.signed("reissued-receipt", carried, receipt_owner))
        self.accept("consume-receipt", plain, additional=receipt_owner)
        assert self.l1.call("get_live_cell", receipt["out_point"], True)["status"] != "live"

        proof = self.conflict(r1, shares[2])
        damaged = proof[:-1] + bytes([proof[-1] ^ 1])
        self.reject("bad-slash-signature", self.slash(2, damaged)[0], "error code 99")
        tx, self.state, self.capacity = self.slash(2, proof)
        self.head = point(self.accept("slash-one", tx), 0)
        self.reject("replayed-slash", self.slash(2, proof)[0], "error code 99")
        attempt = subprocess.run(self.sign_command(r2, 2), capture_output=True, text=True)
        assert attempt.returncode and "active domain position" in attempt.stderr, attempt
        self.rejections["slashed-position-signing"] = attempt.stderr

        second = self.collect("post-slash", r2, (0, 1, 3))
        delivered = self.accept("serve-two", self.service(r2, second))
        self.head = point(delivered, 0)
        self.l1.mine(2)
        live_audit = self.audit("active-live", {"cell": self.head}, live=True)
        r3 = self.open_request("query-refund")
        valid_before_retire = self.collect("pre-retirement", r3, (0, 1, 3))
        self.retire()
        self.reject("service-after-retirement", self.service(r3, valid_before_retire), "error code 98")
        tx, self.state, self.capacity = self.slash(3, self.conflict(r2, second[-1]))
        self.head = point(self.accept("slash-during-challenge", tx), 0)

        finish = self.close((0, 1))
        self.reject("close-before-maturity", finish, "Immature")
        self.l1.mine(self.challenge)
        wrong = copy.deepcopy(finish)
        wrong["outputs"][0]["lock"] = self.release_lock
        self.reject("close-redirected-bond", wrong, "error code 97")
        closed = self.accept("close", finish)
        assert self.l1.call("get_live_cell", self.head, True)["status"] != "live"
        self.l1.mine(max(0, r3["refund"] - int(self.l1.call("get_tip_block_number"), 16)))
        refund = self.pay_transaction([r3["point"]], [output(r3["capacity"], r3["buyer"])], ["0x"], [self.code])
        refund["inputs"][0]["since"] = hex(r3["refund"])
        refund["witnesses"] = [witness(b"\x04")]
        refunded = self.accept("refund-after-closure", refund)
        self.l1.mine(2)
        audits = self.verify_audits(live_audit, reorg=audit_reorg)
        return {"audit": audits, "chain_genesis": self.genesis, "domain": self.domain, "receipt": receipt,
                "closed": closed, "refund": refunded, "transactions": self.committed,
                "rejections": self.rejections}


def use_stem_receipt(args, l1, fixture, vault, spec, spec_path, mint, client):
    from native_bridge_localnet import cell, signing_cell

    ckb = Scenario(args, l1, fixture, args.security_domain, funding=vault["change_point"],
                   network=spec["network_id"]).run()

    def query(path):
        result = client.query(path, prove=True)
        return json.loads(bytes.fromhex(result["value"]))

    previous = query("/stem/bridge-anchor")
    confirmation = spec["bridge"]["confirmations"]
    height = int(l1.call("get_tip_block_number"), 16) - confirmation
    block = l1.call("get_block_hash", hex(height))

    def collect(label, method, extra):
        votes = []
        for index, key in enumerate(fixture.guardian_keys[:3]):
            votes.append(json.loads(run(args.binary, "bridge", method,
                "--genesis", spec_path, "--l1-rpc", l1.url, "--key-file", key,
                "--journal", args.output / f"journal-{index}", *extra)))
        path = args.output / f"security-reference-{label}-votes.json"
        write_json(path, votes)
        return path, votes

    anchors, _ = collect("anchor", "attest-anchor",
        ["--previous-height", str(previous["height"]), "--previous-hash", previous["hash"],
         "--height", str(height)])
    update = json.loads(run(args.binary, "bridge", "advance-anchor", "--observations", anchors))
    refreshed = client.call("broadcast_tx_commit", tx=update)
    assert refreshed["check_tx"]["code"] == 0, refreshed
    accepted = query("/stem/bridge-anchor")
    assert accepted["height"] == height and accepted["hash"] == block, accepted

    request = args.output / "security-reference-request.json"
    write_json(request, l1_reference_request(height, block, (ckb["receipt"]["out_point"], False)))
    observations, votes = collect("service", "attest-reference", ["--request", request])
    live_request = args.output / "security-reference-spent-as-live.json"
    write_json(live_request, l1_reference_request(height, block, (ckb["receipt"]["out_point"], True)))
    spurious = subprocess.run([args.binary, "bridge", "attest-reference", "--genesis", spec_path,
        "--l1-rpc", l1.url, "--key-file", fixture.guardian_keys[0], "--journal", args.output / "journal-0",
        "--request", live_request], text=True, capture_output=True)
    assert spurious.returncode and "no longer live" in spurious.stderr, spurious
    record = binary(votes[0]["records"][0])
    assert all(len(vote["records"]) == 1 and binary(vote["records"][0]) == record for vote in votes)
    assert binary(ckb["receipt"]["data"]) in record
    assert binary(ckb["domain"]["domain"]) in record
    native_lock = script(hex_bytes(ckb_hash(args.native_lock.read_bytes())),
                         fixture.key(fixture.owner)["blake160"], "data")
    lock = args.output / "security-reference-wallet-lock.json"
    context = args.output / "security-reference-wallet-context.json"
    write_json(lock, native_lock)
    deps = [mint["lock_dep"], point(mint["mint_id"], 2)]
    write_json(context, {"cells": [signing_cell(client, p) for p in [mint["payment"], *deps]]})

    def sign(label, selected, attachment):
        guard = script(hex_bytes(ckb_hash(args.reference_script.read_bytes())),
                       hex_bytes(bytes([0]) + ckb_hash(selected)), "data")
        draft = args.output / f"security-reference-{label}-draft.json"
        write_json(draft, {"inputs": [{"out_point": mint["payment"]}],
            "outputs": [cell(mint["payment_capacity"] - FEE, native_lock, types=[guard])],
            "cell_deps": [dependency(p) for p in deps]})
        unsigned = args.output / f"security-reference-{label}-unsigned.json"
        write_json(unsigned, json.loads(run(args.binary, "wallet", "build", "--request", draft)))
        if attachment:
            attached = args.output / f"security-reference-{label}-attached.json"
            write_json(attached, json.loads(run(args.binary, "bridge", "attach-references",
                "--genesis", spec_path, "--transaction", unsigned, "--observations", attachment)))
            unsigned = attached
        signed = json.loads(run(args.binary, "wallet", "sign", "--genesis", spec_path,
            "--transaction", unsigned, "--context", context, "--group-index", "0",
            "--lock", lock, "--key-file", fixture.owner))
        write_json(args.output / f"security-reference-{label}-signed.json", signed)
        return signed

    failures = {"spent_as_live": spurious.stderr}
    for label, record_bytes, attachment in (
        ("missing", record, None), ("tampered", record[:-1] + bytes([record[-1] ^ 1]), observations)
    ):
        rejected = client.call("broadcast_tx_sync", tx=sign(label, record_bytes, attachment))
        assert rejected["code"] == 1 and "exited with 13" in rejected["log"], rejected
        failures[label] = rejected["log"]
    signed = sign("accepted", record, observations)
    finalized = client.call("broadcast_tx_commit", tx=signed)
    assert finalized["check_tx"]["code"] == 0, finalized
    location = client.locations[finalized["hash"]]
    proof = client.query("/stem/execution", height=location["height"],
                         data=hex_bytes(le(location["index"], 4)), prove=True)
    return {"ckb": ckb, "anchor": accepted, "anchor_update": refreshed, "stem": finalized,
            "execution": json.loads(bytes.fromhex(proof["value"])), "rejections": failures,
            "stem_transaction_bytes": len(binary(signed))}

def outpoint(value):
    return binary(value["tx_hash"]) + le(int(value["index"], 16), 4)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("binary", "l1", "contract", "output"):
        parser.add_argument(f"--{arg}", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--l1-port", type=int, default=27220)
    parser.add_argument("--audit-reorg", action="store_true")
    args = parser.parse_args()
    args.output.mkdir(mode=0o700, parents=True)
    l1 = L1(args)
    try:
        report = Scenario(args, l1, Fixtures(args), args.contract).run(audit_reorg=args.audit_reorg)
        write_json(args.output / "security-domain-report.json", report)
        print(f"CKB committed {len(report['transactions'])} security transactions; rejected {len(report['rejections'])}")
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

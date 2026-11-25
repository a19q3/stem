"""Publish a permissionless, directly verified Stem checkpoint on CKB devnet."""

import base64
import json

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output, point, script, script_hash, transaction, wallet_group
from localnet import run, write_json
from stake_localnet import witness


COMMITTEE_PAGE_SIZE = 100
MAX_COMMITTEE_SIZE = 128


def committee_at(rpc, height):
    members, page, total = [], 1, None
    while total is None or len(members) < total:
        response = rpc.call("validators", height=height, page=page, per_page=COMMITTEE_PAGE_SIZE)
        validators = response["validators"]
        reported_total = int(response["total"])
        if (int(response["block_height"]) != height
                or not 1 <= reported_total <= MAX_COMMITTEE_SIZE
                or (total is not None and reported_total != total)
                or len(validators) != int(response["count"])
                or len(validators) != min(COMMITTEE_PAGE_SIZE, reported_total - len(members))):
            raise ValueError(f"inconsistent CometBFT validator page {page} at height {height}")
        total = reported_total
        members.extend(validators)
        page += 1
    addresses = {member["address"] for member in members}
    if len(addresses) != total:
        raise ValueError(f"duplicate CometBFT validator at height {height}")
    return members, page - 1


class Relay:
    """Publish checkpoint updates paid from an independent relayer Cell."""

    def __init__(self, args, l1, fixture, anchor, spec_path, comet_genesis, rpc, *, directory="bridge-checkpoints"):
        self.args, self.l1, self.fixture, self.anchor, self.rpc = args, l1, fixture, anchor, rpc
        self.comet_genesis = comet_genesis
        self.spec_path = spec_path
        self.chain = json.loads(comet_genesis.read_text())["chain_id"]
        self.network = anchor["network"]
        self.native_genesis = binary(json.loads(run(args.binary, "genesis-hash", "--genesis", spec_path, "--header")))
        genesis_id = ckb_hash(b"Stem Header\0" + binary(self.network) + self.native_genesis)
        self.app_hash = ckb_hash(b"Stem AppHash\0" + binary(self.network) + bytes(8) + genesis_id)
        self.root = args.output / directory
        self.root.mkdir()
        self.point = self.lock = self.type = None
        self.funding, self.height = anchor["seed"], -1
        self.publications, self.rejections, self.pages = [], {}, {}
        self.bundle = None

    def header(self, height):
        if height == 0:
            return self.native_genesis
        response = self.rpc.call("abci_query", path=json.dumps("/stem/header"), height=height)["response"]
        if response["code"] != 0 or int(response["height"]) != height:
            raise AssertionError(response)
        return base64.b64decode(response["value"])

    def foreign(self, anchor):
        return Relay(self.args, self.l1, self.fixture, anchor, self.spec_path, self.comet_genesis, self.rpc,
            directory="bridge-foreign-checkpoints")

    def audit_burn(self, label, height, header):
        path = self.args.output / f"{label}-successor.json"
        write_json(path, self.rpc.call("commit", height=height + 1))
        app_hash = ckb_hash(b"Stem AppHash\0" + binary(self.network) + le(height, 8)
                           + ckb_hash(b"Stem Header\0" + binary(self.network) + header))
        verified = run(self.args.proof_verifier, "-genesis", self.comet_genesis, "-commit", path,
                       "-height", str(height), "-app-hash", app_hash.hex())
        return path, ["--comet-genesis", self.comet_genesis, "--proof-verifier", self.args.proof_verifier], verified

    def export(self, label, entries):
        source = self.root / f"{label}-source.json"
        write_json(source, entries)
        bundle = json.loads(run(self.args.proof_verifier, "-genesis", self.comet_genesis, "-direct-bundle", source))
        write_json(self.root / f"{label}-bundle.json", bundle)
        return bundle

    def entry(self, height):
        validators, self.pages[height] = committee_at(self.rpc, height)
        return {"commit": self.rpc.call("commit", height=height), "validators": validators,
                "native_header": hex_bytes(self.header(height - 1))}

    def advance(self, target, secp, *, probe=False):
        if target < self.height:
            raise ValueError("cannot roll back a CKB checkpoint")
        while self.height < target:
            end = min(target, self.height + 8)
            entries = [self.entry(height) for height in range(self.height + 2, end + 2)]
            label = f"{self.root.name}-{end}"
            bundle = self.export(label, entries)
            if bundle["native_height"] != end:
                raise AssertionError(bundle)
            if self.point is None:
                args = (self.anchor["id"] + binary(self.network) + binary(self.anchor["genesis"])
                        + bytes.fromhex(bundle["initial_committee"]) + self.app_hash + self.chain.encode())
                self.type = script(self.anchor["hash"], hex_bytes(args), "data2")
                self.lock = dict(self.type, args=hex_bytes(script_hash(self.type)))
            inputs = [self.point, self.funding] if self.point else [self.funding]
            fee_output = self.l1.call("get_live_cell", self.funding, True)["cell"]["output"]
            available = int(fee_output["capacity"], 16) - FEE
            capacity = 1500 * CKB
            if self.point is None:
                available -= capacity

            def sign(name, proof, tip=None):
                tx = transaction(inputs, [output(capacity, self.lock, self.type), output(available, self.fixture.standard)],
                                 [tip or bundle["tip"], "0x"], [self.anchor["code"], secp])
                tx["witnesses"] = [witness(proof)]
                return self.fixture.sign(name, tx, [wallet_group(len(inputs) - 1, [self.fixture.faucet])])

            raw = binary(bundle["proof"])
            if probe:
                self.probe(sign, raw, bundle)
            signed = sign(label, raw)
            estimate = self.l1.call("estimate_cycles", signed["json"])
            tx_hash = self.l1.send(signed)
            self.point, self.funding = point(tx_hash, 0), point(tx_hash, 1)
            self.height = end
            self.bundle = bundle
            self.publications.append({
                "height": end, "point": self.point, "l1_inclusion": self.l1.prove_committed(tx_hash),
                "estimated_cycles": int(estimate["cycles"], 16), "steps": len(entries), "proof_bytes": len(raw),
                "validator_hashes": [item["commit"]["signed_header"]["header"]["validators_hash"] for item in entries],
            })
        return dependency(self.point)

    def probe(self, sign, raw, bundle):
        if self.height == -1:
            self.rejections["modified_vote"] = self.l1.reject(
                sign("direct-invalid-signature", raw[:-1] + bytes([raw[-1] ^ 1])))
            self.rejections["missing_quorum"] = self.l1.reject(
                sign("direct-zero-quorum", raw[:-80] + bytes(2)))
            wrong_tip = bytearray(binary(bundle["tip"]))
            wrong_tip[-1] ^= 1
            self.rejections["different_execution_root"] = self.l1.reject(
                sign("direct-bad-output-root", raw, hex_bytes(wrong_tip)))
        if self.height == 0:
            later = self.export("skip-height", [self.entry(3)])
            self.rejections["skipped_commit"] = self.l1.reject(
                sign("direct-skip-height", binary(later["proof"]), later["tip"]))
            self.rejections["replayed_commit"] = self.l1.reject(
                sign("direct-replay", binary(self.bundle["proof"]), self.bundle["tip"]))
            tampered = bytearray(raw)
            first_header_len = int.from_bytes(tampered[1:3], "little")
            first_member = 3 + first_header_len + 36 + 4 + 208 + 2
            tampered[first_member + 32] ^= 1
            self.rejections["forged_committee"] = self.l1.reject(sign("direct-forged-committee", bytes(tampered)))

    def report(self):
        return {"type": hex_bytes(script_hash(self.type)), "publications": self.publications,
                "rejections": self.rejections, "committee_pages": self.pages}

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


def publish(args, scenario, stem_genesis, comet_genesis, rpc, header, funding=None):
    root = args.output / "direct-checkpoints"
    root.mkdir()
    l1, fixture = scenario.l1, scenario.fixture
    program = args.direct_checkpoint.read_bytes()
    faucet = funding or point(scenario.code["out_point"]["tx_hash"], 2)
    source = l1.call("get_transaction", faucet["tx_hash"])["transaction"]
    funds = int(source["outputs"][int(faucet["index"], 16)]["capacity"], 16)
    code_capacity, relay_capacity = (len(program) + 200) * CKB, 8_000 * CKB
    deployment = transaction(
        [faucet], [output(cap, fixture.standard) for cap in
                   (code_capacity, relay_capacity, funds - code_capacity - relay_capacity - FEE)],
        [hex_bytes(program), "0x", "0x"], [scenario.secp],
    )
    deployed = l1.send(scenario.sign("direct-checkpoint-deploy", deployment, 0))
    dep = dependency(point(deployed, 0))
    funding = point(deployed, 1)
    type_id = ckb_hash(le(0, 8) + binary(deployed) + le(1, 4) + le(0, 8))
    code_hash = hex_bytes(ckb_hash(program))
    chain = json.loads(comet_genesis.read_text())["chain_id"]
    native_genesis = binary(json.loads(run(args.binary, "genesis-hash", "--genesis", stem_genesis, "--header")))
    genesis_id = ckb_hash(b"Stem Header\0" + scenario.network + native_genesis)
    genesis_app_hash = ckb_hash(b"Stem AppHash\0" + scenario.network + bytes(8) + genesis_id)

    pages = {}

    def entry(height):
        validators, pages[height] = committee_at(rpc, height)
        return {"commit": rpc.call("commit", height=height), "validators": validators,
                "native_header": hex_bytes(native_genesis if height == 1 else header(rpc, height - 1))}

    entries = [entry(height) for height in range(1, 33)]
    write_json(root / "native-stem-and-comet-commits.json", entries)

    def export(label, selected):
        source = root / f"{label}-source.json"
        write_json(source, selected)
        bundle = json.loads(run(args.proof_verifier, "-genesis", comet_genesis, "-direct-bundle", source))
        path = root / f"{label}-bundle.json"
        write_json(path, bundle)
        return path, bundle

    first = export("bootstrap", entries[:1])
    committee = bytes.fromhex(first[1]["initial_committee"])
    policy = type_id + scenario.network + scenario.genesis + committee + genesis_app_hash + chain.encode()
    type_script = script(code_hash, hex_bytes(policy), "data2")
    identity = script_hash(type_script)
    lock = script(code_hash, hex_bytes(identity), "data2")
    capacity = 1500 * CKB
    previous, previous_bundle, previous_tip = None, None, None
    publications, rejections = [], {}
    trust = ["--network", hex_bytes(scenario.network), "--chain-id", chain,
             "--initial-committee", hex_bytes(committee), "--genesis-app-hash", hex_bytes(genesis_app_hash)]

    for offset in (0, 1, 9, 17, 25):
        end = min(len(entries), offset + (1 if offset == 0 else 8))
        bundle_path, bundle = first if offset == 0 else export(f"direct-{end}", entries[offset:end])
        cli = [args.binary, "checkpoint", "verify-direct", *trust, "--bundle", bundle_path]
        if previous_tip is not None:
            cli += ["--previous-tip", previous_tip]
        verified = json.loads(run(*cli))
        if verified["height"] != end - 1 or verified["height"] != bundle["native_height"]:
            raise AssertionError(verified)
        consumed = [] if previous is None else [previous]
        available = relay_capacity - (capacity if previous is None else 0) - FEE

        def sign(label, proof, tip=None):
            tx = transaction(
                [*consumed, funding],
                [output(capacity, lock, type_script), output(available, fixture.standard)],
                [tip or bundle["tip"], "0x"], [scenario.secp, dep],
            )
            tx["witnesses"] = [witness(proof)]
            return scenario.sign(label, tx, len(consumed))

        proof = binary(bundle["proof"])
        if offset == 0:
            rejections["modified_vote"] = l1.reject(sign("direct-invalid-signature", proof[:-1] + bytes([proof[-1] ^ 1])))
            rejections["missing_quorum"] = l1.reject(sign("direct-zero-quorum", proof[:-80] + bytes(2)))
            wrong_tip = bytearray(binary(bundle["tip"]))
            wrong_tip[-1] ^= 1
            rejections["different_execution_root"] = l1.reject(sign("direct-bad-output-root", proof, hex_bytes(wrong_tip)))
        if offset == 1:
            _, later = export("skip-height", entries[2:3])
            rejections["skipped_commit"] = l1.reject(sign("direct-skip-height", binary(later["proof"]), later["tip"]))
            rejections["replayed_commit"] = l1.reject(sign("direct-replay", binary(previous_bundle["proof"]), previous_bundle["tip"]))
            # The first member of the current set has 40 bytes ending at the
            # first vote count. Alter its power without touching the signed header.
            tampered = bytearray(proof)
            first_header_len = int.from_bytes(tampered[1:3], "little")
            member = 3 + first_header_len + 36 + 4 + 208 + 2
            tampered[member + 32] ^= 1
            rejections["forged_committee"] = l1.reject(sign("direct-forged-committee", bytes(tampered)))
        signed = sign(f"direct-publish-{end}", proof)
        estimate = l1.call("estimate_cycles", signed["json"])
        tx_hash = l1.send(signed)
        previous = point(tx_hash, 0)
        previous_bundle = bundle
        previous_tip = root / f"direct-{end}-tip.json"
        write_json(previous_tip, bundle["tip"])
        branch = [entry["commit"]["signed_header"]["header"]["validators_hash"] for entry in entries[offset:end]]
        publications.append({
            "height": end - 1, "steps": end - offset, "proof_bytes": len(proof),
            "estimated_cycles": int(estimate["cycles"], 16), "tx": tx_hash,
            "stem_app_hash": verified["app_hash"], "validator_hashes": branch,
            "ckb_inclusion": l1.prove_committed(tx_hash),
        })
        relay_capacity, funding = available, point(tx_hash, 1)
    if len({item for publication in publications for item in publication["validator_hashes"]}) < 4:
        raise AssertionError("fixture omitted a committee or power transition")
    return {"type": hex_bytes(identity), "initial_committee": hex_bytes(committee),
            "genesis_app_hash": hex_bytes(genesis_app_hash), "publications": publications,
            "rejections": rejections, "committee_pages": pages}


class Relay:
    """Publish checkpoint updates paid from an independent relayer Cell."""

    def __init__(self, args, l1, fixture, anchor, spec_path, comet_genesis, rpc, *, directory="bridge-checkpoints"):
        self.args, self.l1, self.fixture, self.anchor, self.rpc = args, l1, fixture, anchor, rpc
        self.comet_genesis = comet_genesis
        self.chain = json.loads(comet_genesis.read_text())["chain_id"]
        self.network = anchor["network"]
        self.native_genesis = binary(json.loads(run(args.binary, "genesis-hash", "--genesis", spec_path, "--header")))
        genesis_id = ckb_hash(b"Stem Header\0" + binary(self.network) + self.native_genesis)
        self.app_hash = ckb_hash(b"Stem AppHash\0" + binary(self.network) + bytes(8) + genesis_id)
        self.root = args.output / directory
        self.root.mkdir()
        self.point = self.lock = self.type = None
        self.funding, self.height = anchor["seed"], -1
        self.publications = []

    def header(self, height):
        if height == 0:
            return self.native_genesis
        response = self.rpc.call("abci_query", path=json.dumps("/stem/header"), height=height)["response"]
        if response["code"] != 0 or int(response["height"]) != height:
            raise AssertionError(response)
        return base64.b64decode(response["value"])

    def advance(self, target, secp):
        if target < self.height:
            raise ValueError("cannot roll back a CKB checkpoint")
        while self.height < target:
            end = min(target, self.height + 8)
            entries = []
            for height in range(self.height + 2, end + 2):
                validators, _ = committee_at(self.rpc, height)
                entries.append({"commit": self.rpc.call("commit", height=height), "validators": validators,
                                "native_header": hex_bytes(self.header(height - 1))})
            label = f"{self.root.name}-{end}"
            source = self.root / f"{label}-source.json"
            write_json(source, entries)
            bundle = json.loads(run(self.args.proof_verifier, "-genesis", self.comet_genesis, "-direct-bundle", source))
            write_json(self.root / f"{label}-bundle.json", bundle)
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
            tx = transaction(inputs, [output(capacity, self.lock, self.type), output(available, self.fixture.standard)],
                             [bundle["tip"], "0x"], [self.anchor["code"], secp])
            tx["witnesses"] = [witness(binary(bundle["proof"]))]
            signed = self.fixture.sign(label, tx, [wallet_group(len(inputs) - 1, [self.fixture.faucet])])
            tx_hash = self.l1.send(signed)
            self.point, self.funding = point(tx_hash, 0), point(tx_hash, 1)
            self.height = end
            self.publications.append({"height": end, "point": self.point, "l1_inclusion": self.l1.prove_committed(tx_hash)})
        return dependency(self.point)

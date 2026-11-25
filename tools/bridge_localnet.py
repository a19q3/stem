#!/usr/bin/env python3
"""Deploy and check the federated native CKB round trip on isolated local nodes."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import secrets
import subprocess
import time

from localnet import Rpc, await_genesis, export_execution_proof, init_network, run, set_sync_config, wait_height, write_json

CKB = 100_000_000
FEE = 1_000_000
FAUCET_KEY = bytes.fromhex("d00c06bfd800d27397002dca6fb0993d5ba6399b4238b2f29ee9deb97593d2bc")
FAUCET_ARG = "0xc8328aabcd9b9e8e64fbc566c4385c3bdeb219d7"
SIGHASH_CODE = "0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8"


def binary(value):
    return bytes.fromhex(value.removeprefix("0x"))


def hex_bytes(data):
    return "0x" + data.hex()


def ckb_hash(data):
    return hashlib.blake2b(data, digest_size=32, person=b"ckb-default-hash").digest()


def le(number, length):
    return number.to_bytes(length, "little")


def packed_script(script):
    args = binary(script["args"])
    kind = {"data": 0, "type": 1, "data1": 2, "data2": 4}[script["hash_type"]]
    offsets = le(53 + len(args), 4) + le(16, 4) + le(48, 4) + le(49, 4)
    return offsets + binary(script["code_hash"]) + bytes([kind]) + le(len(args), 4) + args


def script_hash(script):
    return ckb_hash(packed_script(script))


def script(code_hash, args="0x", kind="type"):
    return dict(code_hash=code_hash, hash_type=kind, args=args)


def output(capacity, lock, type_=None):
    return {"capacity": hex(capacity), "lock": lock, "type": type_}


def point(tx_hash, index):
    return {"tx_hash": tx_hash, "index": hex(index)}


def dependency(outpoint, dep_type="code"):
    return {"out_point": outpoint, "dep_type": dep_type}


def transaction(inputs, outputs, data, deps):
    return {"version": "0x0", "cell_deps": deps, "header_deps": [],
            "inputs": [{"previous_output": p, "since": "0x0"} for p in inputs],
            "outputs": outputs, "outputs_data": data, "witnesses": []}


def wallet_group(input_index, key_files, prefix="0x"):
    return {"inputs": [input_index], "keys": [str(path) for path in key_files], "lock_prefix": prefix}


def write_key(out, name, raw):
    path = out / f"{name}.key"
    with path.open("xb") as file:
        path.chmod(0o600)
        file.write(raw)
    return path


class CkbRpc(Rpc):
    def call(self, method, *params):
        self.connection.request("POST", "/", json.dumps({"id": 1, "jsonrpc": "2.0",
                                "method": method, "params": params}), {"Content-Type": "application/json"})
        response = json.loads(self.connection.getresponse().read())
        if response.get("error"):
            raise RuntimeError(response["error"])
        return response["result"]


class L1(CkbRpc):
    def __init__(self, args):
        super().__init__(args.l1_port)
        self.url = f"http://127.0.0.1:{args.l1_port}/"
        self.home = args.output / "l1"
        self.home.mkdir(mode=0o700)
        self.process = None
        run(args.l1, "init", "-C", self.home, "--chain", "dev",
            "--rpc-port", str(args.l1_port), "--p2p-port", str(args.l1_port + 1),
            "--ba-arg", FAUCET_ARG, "--genesis-message", "fern-local-bridge")
        config = self.home / "ckb.toml"
        config.write_text(re.sub(r"(?m)^modules = \[(.*)\]",
                                 r'modules = [\1, "IntegrationTest"]', config.read_text(), count=1))
        with (self.home / "l1.log").open("wb", buffering=0) as log:
            self.process = subprocess.Popen([args.l1, "run", "-C", self.home],
                                            stdout=log, stderr=subprocess.STDOUT)
        started = time.monotonic()
        while time.monotonic() - started < 30:
            if self.process.poll() is not None:
                raise RuntimeError(f"L1 exited. See {self.home / 'l1.log'}")
            try:
                self.call("get_tip_block_number")
                break
            except (OSError, RuntimeError):
                self.close()
            time.sleep(0.1)
        else:
            self.stop()
            raise TimeoutError("L1 RPC failed to start")

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.close()

    def send(self, signed):
        tx = self.call("send_transaction", signed["json"], "passthrough")
        if tx != signed["hash"]:
            raise RuntimeError("L1 RPC returned a different transaction hash")
        for _ in range(30):
            result = self.call("get_transaction", tx)
            status = result["tx_status"]["status"]
            if status == "committed":
                return tx
            if status == "rejected":
                raise RuntimeError(result["tx_status"])
            time.sleep(0.10)
            self.call("generate_block")
        raise TimeoutError(f"L1 transaction {tx} did not commit")

    def mine(self, count):
        target = int(self.call("get_tip_block_number"), 16) + count
        deadline = time.monotonic() + 10
        while int(self.call("get_tip_block_number"), 16) < target:
            if time.monotonic() >= deadline:
                raise TimeoutError(f"L1 did not reach height {target}")
            time.sleep(0.10)
            self.call("generate_block")

    def prove_committed(self, tx_hash):
        proof = self.call("get_transaction_and_witness_proof", [tx_hash])
        if self.call("verify_transaction_and_witness_proof", proof) != [tx_hash]:
            raise RuntimeError("L1 witness proof authenticates a different transaction")
        header = self.call("get_header", proof["block_hash"])
        if self.call("get_block_hash", header["number"]) != proof["block_hash"]:
            raise RuntimeError("L1 transaction proof is outside the canonical chain")
        return {"header": header, "proof": proof}

    def reject(self, signed):
        try:
            self.call("send_transaction", signed["json"], "passthrough")
        except RuntimeError as error:
            if "ValidationFailure" not in str(error):
                raise
            return str(error)
        raise RuntimeError("L1 accepted an invalid bridge transition")


class Fixtures:
    def __init__(self, args):
        self.args = args
        self.out = args.output
        self.l1_keys = [write_key(self.out, f"custody-{i}", secrets.token_bytes(32)) for i in range(3)]
        self.guardian_keys = [write_key(self.out, f"guardian-{i}", secrets.token_bytes(32)) for i in range(4)]
        self.faucet = write_key(self.out, "faucet", FAUCET_KEY)
        self.owner = write_key(self.out, "sidechain-owner", bytes([1] * 32))
        self.friend = write_key(self.out, "sidechain-friend", bytes([2] * 32))
        self.standard = script(SIGHASH_CODE, FAUCET_ARG)
        self.owner_lock = script(SIGHASH_CODE, self.key(self.owner)["blake160"])
        self.friend_lock = script(SIGHASH_CODE, self.key(self.friend)["blake160"])
        self.multisig_prefix = bytes([0, 0, 2, 3]) + b"".join(
            binary(self.key(k)["blake160"]) for k in self.l1_keys)
        self.guardians = [{"ed25519": self.key(key, ed=True)["ed25519"], "power": 1}
                          for key in self.guardian_keys]

    def key(self, key, ed=False):
        args = ["--ed25519"] if ed else []
        return json.loads(run(self.args.binary, "ckb-wallet", "key", "--key-file", key, *args))

    def sign(self, name, tx, groups):
        tx_file, grp_file = self.out / f"{name}.json", self.out / f"{name}-groups.json"
        write_json(tx_file, tx)
        write_json(grp_file, groups)
        signed = json.loads(run(self.args.binary, "ckb-wallet", "sign", "--transaction", tx_file,
                                "--groups", grp_file))
        write_json(self.out / f"{name}-signed.json", signed)
        return signed


def vault_state(action, next_nonce, deposit_hash, multisig_hash, amount=0,
                payout_hash=bytes(32), burn_id=bytes(32)):
    return hex_bytes(bytes([1, action]) + le(next_nonce, 8) + binary(deposit_hash)
                     + binary(multisig_hash) + le(amount, 8) + payout_hash + burn_id)


def sidechain_tx(node, signed, required=True):
    rpc = Rpc(node.rpc)
    try:
        check = rpc.call("broadcast_tx_sync", tx=signed)
        if check["code"] != 0:
            if required:
                raise RuntimeError(check)
            return check
        if not required:
            raise RuntimeError("Duplicate mint passed admission")
        return wait_sidechain_tx(rpc, check["hash"])
    finally:
        rpc.close()


def wait_sidechain_tx(rpc, tx_id):
    for _ in range(100):
        try:
            result = rpc.call("tx", hash="0x" + tx_id, prove="true")
            if result["tx_result"]["code"] != 0:
                raise RuntimeError(result["tx_result"])
            return result
        except RuntimeError as error:
            if "not found" not in str(error):
                raise
        time.sleep(0.1)
    raise TimeoutError("Sidechain did not include the transaction")


def query_withdrawal(node, nonce):
    rpc = Rpc(node.rpc)
    try:
        response = rpc.call("abci_query", path='"/withdrawal"', data=hex_bytes(nonce.to_bytes(8, "big")))["response"]
        if response["code"] != 0:
            raise RuntimeError(response)
        return json.loads(base64.b64decode(response["value"]))
    finally:
        rpc.close()


def reject_cli(expected, *command):
    result = subprocess.run(command, text=True, capture_output=True, timeout=20)
    if result.returncode == 0 or expected not in result.stderr:
        raise RuntimeError(f"Expected {expected!r}, got {result.stderr}")
    return result.stderr.splitlines()[0]


def payout_arguments(args, l1, node, proof, journals, vault_input):
    vault_file = args.output / "payout-vault.json"
    write_json(vault_file, vault_input)
    command = [
        "--comet-genesis", node.home / "config" / "genesis.json",
        "--proof-verifier", args.proof_verifier, "--proof", proof,
        "--l1-rpc", l1.url, "--vault", vault_file,
    ]
    if getattr(args, "legacy_withdrawal_receipts", False):
        command += ["--sidechain-rpc", f"http://127.0.0.1:{node.rpc}/"]
    for journal in journals:
        command += ["--journal", journal]
    return command


def payout_command(args, l1, node, proof, journals, vault_input, candidate):
    return [args.binary, "bridge", "check-payout", "--transaction", candidate,
            *payout_arguments(args, l1, node, proof, journals, vault_input)]


def prepare_payout(args, l1, node, proof, journals, inputs, deps, *, typed=False):
    fee_input = args.output / "payout-fee-input.json"
    dep_file = args.output / "payout-deps.json"
    write_json(fee_input, inputs[-1])
    write_json(dep_file, deps)
    command = [args.binary, "bridge", "prepare-payout", "--fee-input", fee_input,
               "--cell-deps", dep_file,
               *payout_arguments(args, l1, node, proof, journals, inputs[0])]
    reject_cli("fee exceeds the fee input capacity", *command, "--fee", str(2**64 - 1))
    command += ["--fee", str(FEE)]
    if typed:
        reject_cli("typed payout requires --escrow", *command)
        escrow = args.output / "payout-escrow.json"
        write_json(escrow, inputs[1])
        command += ["--escrow", escrow]
    return json.loads(run(*command))


def mint_command(args, node, app):
    return [args.binary, "bridge", "mint", "--genesis", app, "--sidechain-rpc", f"http://127.0.0.1:{node.rpc}/"]


def guardian_votes(directory, name, command, keys, journals):
    votes = []
    for index, (key, journal) in enumerate(zip(keys, journals, strict=True)):
        path = directory / f"{name}-{index}.json"
        write_json(path, json.loads(run(*command, "--key-file", key, "--journal", journal)))
        votes.append(path)
    return votes


def guardian_status(node):
    rpc = Rpc(node.rpc)
    try:
        reply = rpc.call("abci_query", path='"/bridge_guardians"')["response"]
        if reply["code"]:
            raise RuntimeError(reply)
        return json.loads(base64.b64decode(reply["value"]))
    finally:
        rpc.close()


def rotate_guardians(args, fixture, node, app, side_dep, admin, command, old_votes):
    initial = guardian_status(node)
    if initial["backing_anchor"] is None:
        raise RuntimeError("The committed first mint did not establish the backing anchor")
    keys = [write_key(args.output, f"incoming-guardian-{i}", secrets.token_bytes(32)) for i in range(4)]
    journals = [args.output / f"incoming-guardian-journal-{i}" for i in range(4)]
    members = [{"ed25519": fixture.key(key, ed=True)["ed25519"], "power": power}
               for key, power in zip(keys, [2, 2, 1, 1])]
    if {member["ed25519"] for member in members} & {member["ed25519"] for member in initial["members"]}:
        raise RuntimeError("The new set must replace the entire deposit guardian committee")
    policy = args.output / "incoming-guardians.json"
    write_json(policy, members)
    data = json.loads(run(args.binary, "bridge", "guardian-data", "--members", policy))
    lock = script(SIGHASH_CODE, fixture.key(admin)["blake160"])
    cap = 12_000 * CKB
    pending = json.loads(run(*mint_command(args, node, app), *old_votes[:3]))
    early = reject_cli("not a configured bridge guardian", *command,
                       "--key-file", keys[0], "--journal", args.output / "early-incoming-journal")
    rotation = transaction([initial["generation"]], [output(cap, lock)], [data], [side_dep])
    wrong = fixture.sign("unauthorized-guardian-rotation", rotation, [wallet_group(0, [fixture.owner])])
    rejected_owner = sidechain_tx(node, wrong["molecule"], required=False)
    if rejected_owner["code"] == 0:
        raise RuntimeError("An unrelated spending key replaced the guardians")
    validator_cell = json.loads(app.read_text())["governance"]
    validator_data = args.output / "initial-validator-data.json"
    write_json(validator_data, json.loads(app.read_text())["validators"])
    validator_data = json.loads(run(args.binary, "committee-data", "--members", validator_data))
    wrong_tag = dict(rotation, outputs_data=[validator_data])
    bad = fixture.sign("validator-data-as-guardian-data", wrong_tag, [wallet_group(0, [admin])])
    tag_failure = sidechain_tx(node, bad["molecule"], required=False)
    if "invalid governance data version" not in tag_failure["log"]:
        raise RuntimeError(f"Guardian capability accepted validator data: {tag_failure}")
    both = transaction([initial["generation"], validator_cell],
                       [output(cap, lock), output(cap, fixture.friend_lock)], [data, validator_data], [side_dep])
    bad = fixture.sign("joint-governance-spend", both, [wallet_group(0, [admin]), wallet_group(1, [fixture.friend])])
    joined_failure = sidechain_tx(node, bad["molecule"], required=False)
    if "require separate transactions" not in joined_failure["log"]:
        raise RuntimeError(f"One transaction spent both governance capabilities: {joined_failure}")

    signed = fixture.sign("guardian-rotation", rotation, [wallet_group(0, [admin])])
    rotated = sidechain_tx(node, signed["molecule"])
    active = guardian_status(node)
    if (active["generation"] != point(signed["hash"], 0) or active["backing_anchor"] != initial["backing_anchor"] or
            sorted(active["members"], key=lambda v: v["ed25519"]) != sorted(members, key=lambda v: v["ed25519"])):
        raise RuntimeError("Rotation did not install the reviewed guardian set with its inherited L1 anchor")
    rejected = sidechain_tx(node, pending["transaction"], required=False)
    if "inactive guardian generation" not in rejected["log"]:
        raise RuntimeError(f"The old certificate remained active: {rejected}")
    retired = reject_cli("not a configured bridge guardian", *command,
                         "--key-file", fixture.guardian_keys[0], "--journal", args.output / "retired-guardian-journal")
    votes = guardian_votes(args.output, "incoming-vote", command, keys, journals)
    threshold = reject_cli("more than two thirds", *mint_command(args, node, app), *votes[:2])
    candidate = json.loads(run(*mint_command(args, node, app), *votes[:3]))

    refresh = transaction([active["generation"]], [output(cap, lock)], [data], [side_dep])
    signed_refresh = fixture.sign("guardian-refresh", refresh, [wallet_group(0, [admin])])
    refreshed = sidechain_tx(node, signed_refresh["molecule"])
    final = guardian_status(node)
    if final["generation"] != point(signed_refresh["hash"], 0) or final["members"] != active["members"]:
        raise RuntimeError("Same-key refresh did not advance the certificate generation")
    rejected_refresh = sidechain_tx(node, candidate["transaction"], required=False)
    if "inactive guardian generation" not in rejected_refresh["log"]:
        raise RuntimeError(f"A same-key refresh did not retire previous certificates: {rejected_refresh}")
    relabelled = []
    for i, vote in enumerate(votes[:3]):
        replacement = args.output / f"relabelled-vote-{i}.json"
        write_json(replacement, dict(json.loads(vote.read_text()), generation=final["generation"]))
        relabelled.append(replacement)
    signature = reject_cli("signature error", *mint_command(args, node, app), *relabelled)
    fresh = guardian_votes(args.output, "refreshed-vote", command, keys, journals)
    stale_backing = []
    for i, vote in enumerate(fresh[:3]):
        replacement = args.output / f"stale-backing-vote-{i}.json"
        write_json(replacement, dict(json.loads(vote.read_text()), previous_backing_anchor=None))
        stale_backing.append(replacement)
    backing_failure = reject_cli("outdated L1 backing anchor", *mint_command(args, node, app), *stale_backing)
    mint = json.loads(run(*mint_command(args, node, app), *fresh[:3]))
    if not pending["ckb_tx_hash"] == candidate["ckb_tx_hash"] == mint["ckb_tx_hash"]:
        raise RuntimeError("Rotating the guardian keys changed the deposit's mint identity")
    for name, receipt, tx in (("guardian-rotation", rotated, signed), ("guardian-refresh", refreshed, signed_refresh)):
        export_execution_proof(node, int(receipt["height"]), tx["hash"], args.output / f"{name}-proof.json")
    return mint, {
        "initial": initial, "rotated": active, "refreshed": final, "early_incoming_rejected": early,
        "unauthorized_owner_rejected": rejected_owner["log"], "validator_tag_rejected": tag_failure["log"],
        "joint_spend_rejected": joined_failure["log"], "stale_certificate_rejected": rejected["log"],
        "retired_guardian_rejected": retired, "exactly_two_thirds_rejected": threshold,
        "same_key_refresh_rejected_old_certificate": rejected_refresh["log"], "relabelled_signature_rejected": signature,
        "outdated_backing_rejected": backing_failure,
        "rotation_height": int(rotated["height"]), "refresh_height": int(refreshed["height"]),
    }


def restore_guardians(args, nodes, public, height):
    source, observer = nodes[0], nodes[-1]
    rpc, joining = Rpc(source.rpc), Rpc(observer.rpc)
    try:
        checkpoint = (height // args.checkpoint_interval + 1) * args.checkpoint_interval
        wait_height(rpc, checkpoint + 2)
        trust = rpc.call("block", height=checkpoint)["block_id"]["hash"]
        set_sync_config(observer, nodes[:2], checkpoint, trust)
        observer.start()
        await_genesis([observer])
        wait_height(joining, checkpoint)
        if f"event=snapshot_installed height={checkpoint}" not in (observer.directory / "app.log").read_text():
            raise RuntimeError("The joining replica did not install the requested guardian checkpoint")
        if guardian_status(observer) != public:
            raise RuntimeError("Snapshot did not restore guardian membership and backing anchor")
        observer.stop(crash=True)
        joining.close()
        observer.start()
        await_genesis([observer])
        if guardian_status(observer) != public:
            raise RuntimeError("Restored guardian membership did not persist across restart")
        for name in ("guardian-rotation", "guardian-refresh"):
            signed = json.loads((args.output / f"{name}-signed.json").read_text())
            response = joining.call("abci_query", path='"/transaction"', data=signed["hash"])["response"]
            if response["code"] or base64.b64decode(response["value"])[32:].hex() != signed["molecule"][2:]:
                raise RuntimeError("The compact snapshot omitted the signed guardian transition")
        return checkpoint
    finally:
        rpc.close()
        joining.close()


def rotate_vault(args, l1, fixture, app, vault, fee_input, deps):
    keys = [write_key(args.output, f"incoming-custody-{index}", secrets.token_bytes(32)) for index in range(3)]
    prefix = bytes([0, 1, 2, 3]) + b"".join(binary(fixture.key(key)["blake160"]) for key in keys)

    def config(prefix):
        return {"require_first_n": prefix[1], "threshold": prefix[2],
                "pubkey_hashes": [hex_bytes(prefix[i:i + 20]) for i in range(4, len(prefix), 20)]}

    old, new = config(fixture.multisig_prefix), config(prefix)
    if set(old["pubkey_hashes"]) & set(new["pubkey_hashes"]):
        raise RuntimeError("The custody migration retained an old member")
    inputs = {"vault": vault, "fee-input": fee_input, "cell-deps": deps,
              "current-multisig": old, "new-multisig": new}
    options = ["--genesis", app, "--l1-rpc", l1.url, "--fee", str(FEE)]
    for name, value in inputs.items():
        path = args.output / f"rotation-{name}.json"
        write_json(path, value)
        options.extend(("--" + name, path))
    prepare = [args.binary, "bridge", "prepare-vault-rotation", *options]
    wrong_old = options.copy()
    wrong_old[wrong_old.index("--current-multisig") + 1] = args.output / "rotation-new-multisig.json"
    rejected_policy = reject_cli("does not match the live vault", args.binary, "bridge", "prepare-vault-rotation", *wrong_old)
    bad = args.output / "rotation-invalid-multisig.json"
    write_json(bad, dict(new, pubkey_hashes=[new["pubkey_hashes"][0]] * 3))
    wrong_new = options.copy()
    wrong_new[wrong_new.index("--new-multisig") + 1] = bad
    rejected_duplicate = reject_cli("duplicate multisig", args.binary, "bridge", "prepare-vault-rotation", *wrong_new)
    tx = json.loads(run(*prepare))
    prior = l1.call("get_live_cell", vault, True)["cell"]
    expected_lock = dict(prior["output"]["lock"], args=hex_bytes(ckb_hash(prefix)[:20]))
    if tx["outputs"][0] != dict(prior["output"], lock=expected_lock):
        raise RuntimeError("Prepared rotation changed the vault's identity or balance")
    candidate = args.output / "rotation-candidate.json"
    changed = json.loads(json.dumps(tx))
    changed["outputs"][0]["capacity"] = hex(int(changed["outputs"][0]["capacity"], 16) - 1)
    changed["outputs"][1]["capacity"] = hex(int(changed["outputs"][1]["capacity"], 16) + 1)
    write_json(candidate, changed)
    check = [args.binary, "bridge", "check-vault-rotation", "--transaction", candidate, *options]
    rejected_change = reject_cli("differs from the reviewed custody migration", *check)
    write_json(candidate, tx)
    checked = json.loads(run(*check))
    signed = fixture.sign("rotate-vault", tx,
                          [wallet_group(0, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
                           wallet_group(1, [fixture.faucet])])
    if signed["hash"] != checked["l1_transaction_hash"] or checked["new_vault"] != point(signed["hash"], 0):
        raise RuntimeError("The rotation signature differs from the reviewed transaction")
    rotated = l1.send(signed)
    l1.mine(2)
    new_vault = l1.call("get_live_cell", point(rotated, 0), True)
    if (new_vault["status"] != "live" or new_vault["cell"]["output"] != tx["outputs"][0]
            or new_vault["cell"]["data"]["content"] != tx["outputs_data"][0]
            or l1.call("get_live_cell", vault, True)["status"] == "live"):
        raise RuntimeError("The canonical vault did not rotate to the reviewed multisig")
    rejected_stale = reject_cli("read live L1 vault", *prepare)
    return rotated, keys, prefix, {
        "checked_rotation": checked, "new_multisig": new, "old_multisig": old,
        "wrong_current_multisig_rejected": rejected_policy, "duplicate_incoming_key_rejected": rejected_duplicate,
        "changed_amount_rejected": rejected_change, "spent_vault_rejected": rejected_stale,
        "l1_rotation_proof": l1.prove_committed(rotated),
    }


def verify_rotated_native(args, l1, fixture, nodes, app, rotation, journals, side_dep,
                          l1_deps, deposit_dep, recipient, l1_recipient, guardian_admin):
    node = nodes[0]
    rotated, keys, prefix, report = rotation
    if report["checked_rotation"]["next_withdrawal_nonce"] != 1:
        raise RuntimeError("Rotation changed the pending nonce after the first release")
    vault = l1.call("get_live_cell", point(rotated, 0), True)["cell"]["output"]
    fee_cell = l1.call("get_live_cell", point(rotated, 1), True)["cell"]["output"]
    memo = json.loads(run(args.binary, "bridge", "deposit-cell", "--genesis", app, "--recipient", recipient))
    deposit_amount = 650 * CKB
    fee_change = int(fee_cell["capacity"], 16) - deposit_amount - FEE
    deposit = transaction([point(rotated, 1)],
                          [output(deposit_amount, memo["lock"]), output(fee_change, fixture.standard)],
                          [memo["data"], "0x"], [l1_deps[-1]])
    deposited = l1.send(fixture.sign("rotated-deposit", deposit, [wallet_group(0, [fixture.faucet])]))
    before = int(vault["capacity"], 16)
    consolidation = transaction([point(rotated, 0), point(deposited, 0), point(deposited, 1)],
                                [output(before + deposit_amount, vault["lock"], vault["type"]),
                                 output(fee_change - FEE, fixture.standard)],
                                [vault_state(0, 1, memo["lock"]["code_hash"], vault["lock"]["code_hash"]), "0x"],
                                [*l1_deps, deposit_dep])
    fee_signer = wallet_group(2, [fixture.faucet])
    old = [wallet_group(0, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)), fee_signer]
    rejected_old = l1.reject(fixture.sign("old-custody-consolidation", consolidation, old))
    if "Inputs[0].Lock" not in rejected_old:
        raise RuntimeError(f"Expected the rotated lock to reject the prior signers: {rejected_old}")
    missing = [wallet_group(0, keys[1:], hex_bytes(prefix)), fee_signer]
    rejected_missing = l1.reject(fixture.sign("missing-required-custodian", consolidation, missing))
    if "Inputs[0].Lock" not in rejected_missing:
        raise RuntimeError(f"Expected the rotated lock to enforce its first signer: {rejected_missing}")
    signs = [wallet_group(0, keys[:2], hex_bytes(prefix)), fee_signer]
    consolidated = l1.send(fixture.sign("rotated-consolidation", consolidation, signs))
    l1.mine(2)
    command = [args.binary, "bridge", "attest", "--genesis", app, "--l1-rpc", l1.url,
               "--sidechain-rpc", f"http://127.0.0.1:{node.rpc}/", "--deposit-tx", deposited,
               "--output-index", "0", "--consolidation-tx", consolidated]
    votes = guardian_votes(args.output, "rotated-vote", command, fixture.guardian_keys, journals)
    if guardian_admin:
        mint, guardian_round = rotate_guardians(args, fixture, node, app, side_dep, guardian_admin, command, votes)
        report["guardian_rotation"] = guardian_round
    else:
        mint = json.loads(run(*mint_command(args, node, app), *votes[:3]))
    minted = sidechain_tx(node, mint["transaction"])
    burn_cell = json.loads(run(args.binary, "bridge", "burn-cell", "--genesis", app, "--l1-recipient", l1_recipient))
    claim = deposit_amount - FEE
    burn = transaction([point(mint["ckb_tx_hash"], 0)], [output(claim, burn_cell["lock"])], [burn_cell["data"]], [side_dep])
    signed = fixture.sign("rotated-burn", burn, [wallet_group(0, [fixture.owner])])
    receipt = sidechain_tx(node, signed["molecule"])
    withdrawal = query_withdrawal(node, 1)
    if withdrawal["out_point"] != point(signed["hash"], 0):
        raise RuntimeError("The rotated vault's next release must use withdrawal slot one")
    proof = args.output / "rotated-withdrawal-proof.json"
    export_execution_proof(node, int(receipt["height"]), signed["hash"], proof)
    inputs = [point(consolidated, 0), point(consolidated, 1)]
    release = prepare_payout(args, l1, node, proof, journals, inputs, l1_deps)
    checked_path = args.output / "rotated-release-candidate.json"
    write_json(checked_path, release)
    checked = json.loads(run(*payout_command(args, l1, node, proof, journals, inputs[0], checked_path)))
    signed = fixture.sign("rotated-release", release, [wallet_group(0, keys[:2], hex_bytes(prefix)),
                                                     wallet_group(1, [fixture.faucet])])
    if checked["l1_transaction_hash"] != signed["hash"]:
        raise RuntimeError("Migrated custody signed a different transaction")
    released = l1.send(signed)
    for index, expected in enumerate(release["outputs"][:2]):
        cell = l1.call("get_live_cell", point(released, index), True)
        if (cell["status"], cell["cell"]["output"], cell["cell"]["data"]["content"]) != (
                "live", expected, release["outputs_data"][index]):
            raise RuntimeError("Post-rotation payout or vault state differs")
    state = binary(release["outputs_data"][1])
    if int.from_bytes(state[2:10], "little") != 2 or int(release["outputs"][1]["capacity"], 16) != before + FEE:
        raise RuntimeError("Post-rotation nonce or conservation check failed")
    if guardian_admin:
        expected = json.loads((args.output / "refreshed-vote-0.json").read_text())["deposit"]
        active = guardian_status(node)
        if active["backing_anchor"] != {"height": expected["confirmed_height"], "hash": expected["confirmed_block"]}:
            raise RuntimeError("Executing a mint under the refreshed guardians did not advance the L1 backing anchor")
        guardian_round["after_mint"] = active
        guardian_round["verified_snapshot_height"] = restore_guardians(args, nodes, active, int(receipt["height"]))
    return dict(report, l1_deposit=deposited, l1_consolidation=consolidated, mint_height=int(minted["height"]),
                l1_release=released, withdrawal_slot=1, new_vault_slot=2, released_shannons=claim,
                prior_signers_rejected=rejected_old, missing_required_signer_rejected=rejected_missing,
                l1_release_proof=l1.prove_committed(released))


def scenario(args, l1, fixture):
    genesis = l1.call("get_block_by_number", "0x0")
    genesis_tx, groups = genesis["transactions"][:2]
    funds_idx, funds = next((idx, out) for idx, out in enumerate(genesis_tx["outputs"])
                           if out["lock"]["args"] == FAUCET_ARG)
    l1_secp = dependency(point(groups["hash"], 0), "dep_group")
    l1_multisig = dependency(point(groups["hash"], 1), "dep_group")
    multisig_hash = hex_bytes(script_hash(genesis_tx["outputs"][4]["type"]))
    vault_lock = script(multisig_hash, hex_bytes(ckb_hash(fixture.multisig_prefix)[:20]))
    binaries = [args.vault.read_bytes(), args.deposit_lock.read_bytes()]
    contract_funds = [(len(binary_) + 200) * CKB for binary_ in binaries]
    fee_funds = 12_000 * CKB
    change = int(funds["capacity"], 16) - sum(contract_funds) - 4 * fee_funds - FEE
    tx = transaction([point(genesis_tx["hash"], funds_idx)],
                     [output(c, fixture.standard) for c in contract_funds + [fee_funds] * 4 + [change]],
                     list(map(hex_bytes, binaries)) + ["0x"] * 5, [l1_secp])
    deployed = l1.send(fixture.sign("deploy", tx, [wallet_group(0, [fixture.faucet])]))
    vault_dep = dependency(point(deployed, 0))
    deposit_dep = dependency(point(deployed, 1))
    deposit_code = hex_bytes(ckb_hash(binaries[1]))
    vault_args = hex_bytes(ckb_hash(le(0, 8) + binary(deployed) + le(2, 4) + le(0, 8)))
    vault_type = script(hex_bytes(ckb_hash(binaries[0])), vault_args, "data1")
    vault_hash = hex_bytes(script_hash(vault_type))
    reserve = 1_600 * CKB
    state = vault_state(0, 0, deposit_code, multisig_hash)
    tx = transaction([point(deployed, 2)],
                     [output(reserve, vault_lock, vault_type), output(fee_funds - reserve - FEE, fixture.standard)],
                     [state, "0x"], [vault_dep, l1_secp])
    created = l1.send(fixture.sign("vault", tx, [wallet_group(0, [fixture.faucet])]))
    chain_config = {"l1_genesis": genesis["header"]["hash"], "deposit_lock": script(deposit_code, vault_hash, "data1"),
                    "vault_type_hash": vault_hash, "confirmations": 2, "guardians": fixture.guardians}
    guardian_admin = None
    guardian_lock = None
    consensus_lock = None
    if args.rotate_guardians:
        guardian_admin = write_key(args.output, "guardian-policy-owner", secrets.token_bytes(32))
        guardian_lock = args.output / "guardian-owner-lock.json"
        consensus_lock = args.output / "validator-owner-lock.json"
        write_json(guardian_lock, script(SIGHASH_CODE, fixture.key(guardian_admin)["blake160"]))
        write_json(consensus_lock, fixture.friend_lock)
        args.checkpoint_interval = 4
        args.checkpoint_cell_proofs = args.checkpoint_compact_transactions = True
        args.checkpoint_live_cell_replay = args.compact_transaction_history = True
    nodes, side = init_network(args, bridge=chain_config, governance_lock=consensus_lock,
                               guardian_governance_lock=guardian_lock, observers=int(args.rotate_guardians))
    try:
        validators = nodes[:args.replicas]
        for node in validators:
            node.start()
        await_genesis(validators)
        # User deposit and consolidation to the canonical type-ID vault.
        app_state = args.output / "app-state.json"
        recipient = args.output / "recipient.json"
        write_json(recipient, fixture.owner_lock)
        descriptor = json.loads(run(args.binary, "bridge", "deposit-cell", "--genesis",
                                    app_state, "--recipient", recipient))
        amount = 1_200 * CKB
        tx = transaction([point(deployed, 3)],
                         [output(amount, descriptor["lock"]), output(fee_funds - amount - FEE, fixture.standard)],
                         [descriptor["data"], "0x"], [l1_secp])
        deposited = l1.send(fixture.sign("deposit", tx, [wallet_group(0, [fixture.faucet])]))
        unauthorized = transaction([point(deposited, 0)],
                                   [output(amount - FEE, fixture.standard)], ["0x"], [deposit_dep])
        unauthorized_rejection = l1.reject(fixture.sign("unauthorized-deposit-spend", unauthorized, []))
        pre = reserve + amount
        tx = transaction([point(created, 0), point(deposited, 0), point(deployed, 4)],
                         [output(pre, vault_lock, vault_type), output(fee_funds - FEE, fixture.standard)],
                         [state, "0x"], [vault_dep, deposit_dep, l1_multisig, l1_secp])
        groups_to_sign = [wallet_group(0, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
                          wallet_group(2, [fixture.faucet])]
        consolidated = l1.send(fixture.sign("consolidate", tx, groups_to_sign))
        attest = [args.binary, "bridge", "attest", "--genesis", app_state, "--l1-rpc", l1.url,
                  "--sidechain-rpc", f"http://127.0.0.1:{nodes[0].rpc}/", "--deposit-tx", deposited,
                  "--output-index", "0", "--consolidation-tx", consolidated]
        journals = [args.output / f"guardian-journal-{index}" for index in range(len(fixture.guardian_keys))]
        shallow = reject_cli("insufficient confirmations", *attest, "--key-file", fixture.guardian_keys[0],
                             "--journal", journals[0])
        l1.mine(chain_config["confirmations"])
        votes = guardian_votes(args.output, "vote", attest, fixture.guardian_keys, journals)
        mint_call = mint_command(args, nodes[0], app_state)
        insufficient = reject_cli("more than two thirds", *mint_call, *votes[:2])
        mint = json.loads(run(*mint_call, *votes[:3]))
        alternate = json.loads(run(*mint_call, *votes))
        if alternate["ckb_tx_hash"] != mint["ckb_tx_hash"] or alternate["transaction"] == mint["transaction"]:
            raise RuntimeError("Alternate attestation did not preserve the mint identity")
        minted = sidechain_tx(nodes[0], mint["transaction"])
        rejected_duplicate = sidechain_tx(nodes[0], alternate["transaction"], required=False)
        secp_dep = dependency(point(side["deposit_dep_group"], 0), "dep_group")
        payment = transaction([point(mint["ckb_tx_hash"], 0)], [output(amount - FEE, fixture.friend_lock)],
                              ["0x"], [secp_dep])
        signed_payment = fixture.sign("payment", payment, [wallet_group(0, [fixture.owner])])
        transferred = sidechain_tx(nodes[0], signed_payment["molecule"])
        l1_recipient = args.output / "l1-recipient.json"
        write_json(l1_recipient, fixture.friend_lock)
        descriptor = json.loads(run(args.binary, "bridge", "burn-cell", "--genesis", app_state,
                                    "--l1-recipient", l1_recipient))
        claimed = amount - 2 * FEE
        burn = transaction([point(signed_payment["hash"], 0)], [output(claimed, descriptor["lock"])],
                           [descriptor["data"]], [secp_dep])
        signed_burn = fixture.sign("burn", burn, [wallet_group(0, [fixture.friend])])
        burned = sidechain_tx(nodes[0], signed_burn["molecule"])
        withdrawal = query_withdrawal(nodes[0], 0)
        if withdrawal["out_point"] != point(signed_burn["hash"], 0) or withdrawal["capacity"] != claimed:
            raise RuntimeError("Sidechain withdrawal differs from the signed burn")
        rpc = Rpc(nodes[0].rpc)
        try:
            height = int(burned["height"])
            until = time.monotonic() + 15
            while int(rpc.call("status")["sync_info"]["latest_block_height"]) <= height:
                if time.monotonic() >= until:
                    raise TimeoutError("Missing successor commit for the withdrawal")
                time.sleep(0.1)
            proof = args.output / "withdrawal-proof.json"
            write_json(proof, {"commit": rpc.call("commit", height=height),
                               "next_commit": rpc.call("commit", height=height + 1),
                               "tx": burned, "results": rpc.call("block_results", height=height)})
        finally:
            rpc.close()
        verification = run(args.proof_verifier, "-proof", proof, "-genesis",
                           nodes[0].home / "config" / "genesis.json")
        check_anchor = [args.binary, "bridge", "check-anchor", "--genesis", app_state,
                        "--l1-rpc", l1.url, "--journal", journals[0]]
        anchor_height = json.loads(run(*check_anchor))["height"]
        release_inputs = [point(consolidated, 0), point(deployed, 5)]
        tx = prepare_payout(args, l1, nodes[0], proof, journals, release_inputs,
                            [vault_dep, l1_multisig, l1_secp])
        if tx["outputs"] != [output(claimed, withdrawal["l1_lock"]),
                             output(pre - claimed, vault_lock, vault_type),
                             output(fee_funds - FEE, fixture.standard)]:
            raise RuntimeError("Prepared payout must preserve the expected recipient, vault and change")
        candidate = args.output / "release.json"
        write_json(candidate, tx)
        bad_receipt = None
        if not args.legacy_withdrawal_receipts:
            changed = json.loads(proof.read_text())
            index = int(changed["tx"]["index"])
            receipt = base64.b64decode(changed["tx"]["tx_result"]["data"])
            if len(receipt) != 40 or int.from_bytes(receipt[32:], "little") != 0:
                raise RuntimeError("Burn receipt must commit to the L1 vault's first withdrawal nonce")
            invalid = base64.b64encode(receipt[:32] + le(1, 8)).decode("ascii")
            changed["tx"]["tx_result"]["data"] = invalid
            changed["results"]["txs_results"][index]["data"] = invalid
            tampered_proof = args.output / "tampered-withdrawal-proof.json"
            write_json(tampered_proof, changed)
            bad_receipt = reject_cli("execution results or application hash differ", *payout_command(
                args, l1, nodes[0], tampered_proof, journals, point(consolidated, 0), candidate))
        authorization = json.loads(run(*payout_command(
            args, l1, nodes[0], proof, journals, point(consolidated, 0), candidate)))
        signed_release = fixture.sign("release", tx,
                                      [wallet_group(0, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
                                       wallet_group(1, [fixture.faucet])])
        if authorization["l1_transaction_hash"] != signed_release["hash"]:
            raise RuntimeError("Native payout signature must bind the checked transaction")
        release = l1.send(signed_release)
        result = l1.call("get_live_cell", point(release, 0), True)
        if result["status"] != "live" or result["cell"]["output"] != tx["outputs"][0]:
            raise RuntimeError("L1 payout does not match the sidechain burn")
        post_release = pre - claimed
        replay_state = vault_state(1, 2, deposit_code, multisig_hash, claimed,
                                   script_hash(withdrawal["l1_lock"]), binary(withdrawal["withdrawal_id"]))
        next_inputs = [point(release, 1), point(release, 2)]
        replay = transaction(next_inputs,
                             [output(claimed, withdrawal["l1_lock"]),
                              output(post_release - claimed, vault_lock, vault_type),
                              output(fee_funds - 2 * FEE, fixture.standard)],
                             ["0x", replay_state, "0x"], [vault_dep, l1_multisig, l1_secp])
        release_signers = [wallet_group(0, fixture.l1_keys[:2], hex_bytes(fixture.multisig_prefix)),
                           wallet_group(1, [fixture.faucet])]
        replay_rejection = l1.reject(fixture.sign("replayed-withdrawal", replay, release_signers))
        l1_deps = [vault_dep, l1_multisig, l1_secp]
        rotation = rotate_vault(args, l1, fixture, app_state, *next_inputs, l1_deps)
        migrated = verify_rotated_native(args, l1, fixture, nodes, app_state, rotation, journals, secp_dep,
                                         l1_deps, deposit_dep, recipient, l1_recipient, guardian_admin)
        if args.rotate_guardians:
            retry = json.loads(run(*mint_call, "--submit-rpc", f"http://127.0.0.1:{nodes[0].rpc}/", *votes[:3]))
            if retry["ckb_tx_hash"] != mint["ckb_tx_hash"] or not retry["execution"]["already_finalized"]:
                raise RuntimeError("Retrying a previously committed mint did not remain idempotent across rotation")
            migrated["guardian_rotation"]["retired_mint_retry"] = retry["execution"]
        anchor_height = json.loads(run(*check_anchor))["height"]
        report = {"l1_genesis": chain_config["l1_genesis"], "vault": created, "deposit": deposited,
                  "consolidation": consolidated, "mint_height": minted["height"], "minted_capacity": amount,
                  "duplicate_mint_code": rejected_duplicate["code"], "payment_height": transferred["height"],
                  "burn_height": burned["height"], "withdrawal": withdrawal, "local_commit_verification": verification,
                  "payout_authorization": authorization, "payout_prepared_by_cli": True,
                  "tampered_withdrawal": bad_receipt,
                  "l1_release": release, "l1_payout_capacity": claimed, "remaining_vault_capacity": pre - claimed,
                  "l1_rotated_vault": rotation[0], "custody_migration": migrated,
                  "unauthorized_deposit_spend": unauthorized_rejection,
                  "shallow_deposit": shallow, "insufficient_guardian_power": insufficient,
                  "l1_duplicate_withdrawal": replay_rejection,
                  "l1_version": run(args.l1, "--version")}
        if args.check_reorg:
            # Isolated dev-only fault: replace the previously attested anchor.
            old_anchor = l1.call("get_block_hash", hex(anchor_height))
            old_header = l1.call("get_header", old_anchor)
            l1.call("truncate", l1.call("get_block_hash", hex(anchor_height - 1)))
            template = l1.call("get_block_template")
            template["current_time"] = hex(max(int(template["current_time"], 16),
                                               int(old_header["timestamp"], 16) + 1))
            l1.call("generate_block_with_template", template)
            replacement = l1.call("get_block_hash", hex(anchor_height))
            if replacement == old_anchor:
                raise RuntimeError("L1 did not create a different canonical anchor")
            report["guardian_deep_reorg"] = reject_cli("previous L1 anchor was reorganized", *check_anchor)
            if args.rotate_guardians:
                migrated["guardian_rotation"]["fresh_journal_reorg_rejected"] = reject_cli(
                    "committed backing anchor does not match L1", *attest,
                    "--key-file", args.output / "incoming-guardian-0.key",
                    "--journal", args.output / "fresh-incoming-after-reorg")
            report["l1_rolled_back_after_round_trip"] = True
        write_json(args.output / "round-trip.json", report)
        print(json.dumps(report, indent=2))
    finally:
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "l1", "vault", "deposit-lock", "proof-verifier", "output"):
        parser.add_argument("--" + flag, type=Path, required=True)
    parser.add_argument("--l1-port", type=int, default=28300)
    parser.add_argument("--base-port", type=int, default=28400)
    parser.add_argument("--check-reorg", action="store_true")
    parser.add_argument("--legacy-withdrawal-receipts", action="store_true")
    parser.add_argument("--rotate-guardians", action="store_true")
    args = parser.parse_args()
    for name in ("binary", "comet", "l1", "vault", "deposit_lock", "proof_verifier", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.replicas = 4
    args.vm_workers = 2
    args.commit_ms = 200
    args.idle_interval = "3s"
    args.rate = 1
    args.seconds = 1
    args.output.mkdir(mode=0o700)
    fixtures = Fixtures(args)
    l1 = L1(args)
    try:
        scenario(args, l1, fixtures)
    finally:
        l1.stop()


if __name__ == "__main__":
    main()

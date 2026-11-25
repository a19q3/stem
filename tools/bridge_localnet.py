"""Shared CKB devnet, transaction, and signing helpers."""

import hashlib
import json
from pathlib import Path
import re
import secrets
import subprocess
import time

from localnet import Rpc, run, write_json

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
            "--ba-arg", FAUCET_ARG, "--genesis-message", "stem-local-bridge")
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


def reject_cli(expected, *command):
    result = subprocess.run(command, text=True, capture_output=True, timeout=20)
    if result.returncode == 0 or expected not in result.stderr:
        raise RuntimeError(f"Expected {expected!r}, got {result.stderr}")
    return result.stderr.splitlines()[0]

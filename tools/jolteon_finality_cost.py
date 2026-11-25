#!/usr/bin/env python3
"""Measure the actual Jolteon matrix proofs under the vendored CKB-VM."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path
import re
import subprocess


def ckb_hash(data):
    return hashlib.blake2b(data, digest_size=32, person=b"ckb-default-hash").digest()


def command_args(round_, runner, elf):
    evidence = round_["evidence"]
    spec = evidence["genesis.json"]
    network = spec["network_id"].removeprefix("0x")
    encoded_genesis = evidence["public-genesis-header.json"].removeprefix("0x")
    genesis = ckb_hash(b"Stem Header\0" + bytes.fromhex(network + encoded_genesis)).hex()
    members = "".join(
        member["ed25519"].removeprefix("0x") + member["power"].to_bytes(8, "little").hex()
        for member in spec["development_validators"]
    )
    proof = evidence["two-chain.json"]
    header = evidence["jolteon-report.json"]["proof_verification"]["header"]
    return [str(runner), str(elf), network, genesis, members, proof, header]


def run(args, expected):
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode != expected:
        raise RuntimeError(result.stdout + result.stderr)
    cycles = re.search(r"cycles=(\d+)", result.stdout)
    if cycles is None:
        raise RuntimeError("missing instruction cycles in CKB-VM output")
    return {"exit": result.returncode, "cycles": int(cycles[1]), "stdout": result.stdout}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("runner", "elf", "archive"):
        parser.add_argument(f"--{arg}", type=lambda name: Path(name).resolve(), required=True)
    args = parser.parse_args()
    with lzma.open(args.archive, "rt") as stream:
        archive = json.load(stream)

    rounds = []
    for size in (4, 16, 32, 64, 100, 128):
        command = command_args(archive["rounds"][f"jolteon-{size}-nominal"], args.runner, args.elf)
        result = run(command, 0)
        rounds.append({"members": size, "proof_bytes": len(command[-2]) // 2, **result})
        if size == 4:
            corrupt = bytearray.fromhex(command[-2])
            corrupt[-1] ^= 1
            command[-2] = corrupt.hex()
            rounds.append({"members": size, "case": "changed Ed25519 signature", **run(command, 1)})
    print(json.dumps(rounds, indent=2))


if __name__ == "__main__":
    main()

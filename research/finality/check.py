"""Offline evidence checker for a fixed corpus of authenticated finality observations."""

import argparse
import hashlib
import itertools
import json
import lzma
from pathlib import Path
import subprocess
import tempfile


def intersection(weights):
    # For every pair of signer sets with powers a,b retain the smallest overlap.
    states = {(0, 0): 0}
    for w in weights:
        next_states = {}
        for (a, b), overlap in states.items():
            for left, right in itertools.product((0, 1), repeat=2):
                key = (a + left * w, b + right * w)
                shared = overlap + left * right * w
                next_states[key] = min(shared, next_states.get(key, shared))
        states = next_states
    total = sum(weights)
    smallest = min(
        shared
        for (a, b), shared in states.items()
        if 3 * a > 2 * total and 3 * b > 2 * total
    )
    assert 3 * smallest > total
    return smallest


def consistency(observations):
    accepted = {}
    for header in observations:
        old = accepted.setdefault(header["height"], header["app_hash"])
        if old != header["app_hash"]:
            raise ValueError("inconsistent certified observations at one height")
    return accepted


def digest(domain, network, *fields):
    return hashlib.blake2b(
        domain + network + b"".join(fields), digest_size=32, person=b"ckb-default-hash"
    ).digest()


def requested_header(record, authenticated, network):
    assert authenticated == record.get("anchor", record["header"])
    cursor = authenticated
    for parent in record.get("ancestry", []):
        raw = bytes.fromhex(parent["header"])
        parent_id = digest(b"Stem Header\0", network, raw)
        assert len(raw) == 208 and int.from_bytes(raw[:8], "little") == parent["height"]
        assert parent["height"] + 1 == cursor["height"]
        assert bytes.fromhex(cursor["header"])[16:48] == parent_id
        assert digest(b"Stem AppHash\0", network, raw[:8], parent_id).hex() == parent["app_hash"]
        cursor = parent
    assert cursor == record["header"]
    return cursor


def check(bundle, binary):
    report = bundle["report"]
    spec = bundle["genesis"]
    network = bytes.fromhex(spec["network_id"].removeprefix("0x"))
    headers = []
    previous = None
    with tempfile.TemporaryDirectory() as folder:
        root = Path(folder)
        (root / "genesis.json").write_text(json.dumps(spec))
        for record, proof in zip(
            report["checkpoint_proofs"], bundle["proofs"], strict=True
        ):
            (root / "proof.json").write_text(json.dumps(proof))
            raw = subprocess.check_output(
                [
                    binary,
                    "jolteon-proof",
                    "--genesis",
                    root / "genesis.json",
                    "--proof",
                    root / "proof.json",
                ]
            )
            verified = requested_header(record, json.loads(raw), network)
            header = bytes.fromhex(verified["header"])
            height = int.from_bytes(header[:8], "little")
            assert len(header) == 208 and height == verified["height"]
            if previous:
                assert height == headers[-1]["height"] + 1 and header[16:48] == previous
            previous = digest(b"Stem Header\0", network, header)
            assert (
                digest(b"Stem AppHash\0", network, header[:8], previous).hex()
                == verified["app_hash"]
            )
            headers.append(verified)
    assert headers[-1] == report["agreement"]
    orders = 0
    for perm in itertools.permutations(headers):
        for duplicate in perm:
            assert consistency([*perm, duplicate]) == consistency(headers)
            orders += 1
    # Mutation of the auditor's in-memory output only, never passed into a node.
    try:
        consistency([headers[0], dict(headers[0], app_hash="mismatched observation")])
    except ValueError:
        pass
    else:
        raise AssertionError("inconsistent audit record was accepted")
    assert all(
        node["committed_transactions"] == report["payments"]
        for node in report["resumed"]
    )
    return dict(
        nodes=report["validators"],
        weights=report["weights"],
        offline=report["offline"],
        confirmations=report["payments"],
        height=report["agreement"]["height"],
        min_quorum_intersection=intersection(report["weights"]),
        verified_headers=len(headers),
        observation_orders=orders,
        rejected_audit_mutant=True,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    parser.add_argument(
        "observations", type=Path, nargs="?",
        default=Path(__file__).with_name("observations.json.xz"),
    )
    args = parser.parse_args()
    with lzma.open(args.observations, "rt") as f:
        print(json.dumps([check(b, args.binary) for b in json.load(f)], sort_keys=True))

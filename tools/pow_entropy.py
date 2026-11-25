#!/usr/bin/env python3
"""Derive an experimental epoch seed from a caller-pinned, confirmed CKB Header."""

import argparse
import hashlib
import json
import urllib.request


def hash_bytes(value):
    raw = bytes.fromhex(value.removeprefix("0x"))
    if len(raw) != 32 or raw == bytes(32):
        raise ValueError("expected a nonzero 32-byte hash")
    return raw


def request(url, method, *params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as reply:
        value = json.load(reply)
    if value.get("error") or value.get("result") is None:
        raise ValueError(f"{method} did not return a Header")
    return value["result"]


def derive(call, network, epoch, height, anchor, confirmations):
    if not 0 < confirmations <= 256 or height < 0 or epoch < 0:
        raise ValueError("require positive bounded confirmations and nonnegative epoch and height")
    tip = call("get_tip_header")
    if int(tip["number"], 16) < height + confirmations:
        raise ValueError("anchor is not confirmed")
    genesis = hash_bytes(call("get_header_by_number", "0x0")["hash"])
    previous = anchor
    for number in range(height, height + confirmations + 1):
        header = call("get_header_by_number", hex(number))
        current = hash_bytes(header["hash"])
        if int(header["number"], 16) != number or (
            current != anchor if number == height else hash_bytes(header["parent_hash"]) != previous
        ):
            raise ValueError("anchor or confirmed ancestry changed")
        previous = current
    # Reject a mixed view if the observed confirmation block was replaced.
    confirmed = call("get_header_by_number", hex(height + confirmations))
    if hash_bytes(confirmed["hash"]) != previous:
        raise ValueError("confirmation block changed during observation")
    seed = hashlib.blake2b(
        b"Stem PoW entropy experiment\0" + network + genesis + epoch.to_bytes(8, "little")
        + height.to_bytes(8, "little") + anchor,
        digest_size=32, person=b"ckb-default-hash",
    ).hexdigest()
    return {"epoch": epoch, "height": height, "confirmations": confirmations,
            "ckb_genesis": "0x" + genesis.hex(), "anchor": "0x" + anchor.hex(),
            "confirmation_block": confirmed["hash"], "seed": "0x" + seed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc", required=True)
    parser.add_argument("--network-id", required=True, type=hash_bytes)
    parser.add_argument("--epoch", required=True, type=int)
    parser.add_argument("--height", required=True, type=int)
    parser.add_argument("--anchor", required=True, type=hash_bytes)
    parser.add_argument("--confirmations", required=True, type=int)
    args = parser.parse_args()
    result = derive(lambda *values: request(args.rpc, *values), args.network_id, args.epoch,
                    args.height, args.anchor, args.confirmations)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()

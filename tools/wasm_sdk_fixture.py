"""Capture public wallet and proof inputs from the live Intercell fixture."""

import json

from bridge_localnet import binary, hex_bytes, le
from localnet import run, write_json
from native_bridge_localnet import wait_for


def packed(point):
    return hex_bytes(binary(point["tx_hash"]) + le(int(point["index"], 16), 4))


def observe(client, path, *, data="0x", height=0):
    reply = client.query(path, data=data, height=height, prove=True)
    return reply["height"], json.loads(bytes.fromhex(reply["value"]))


def authenticate(client, height):
    header = client.header(height)
    proof = client.finality(height)
    assert "proof" in proof and not proof["handoff"], proof
    return {"header": header, "finality": proof["proof"]}


def signing_fixture(args, client, spec, points, unsigned, signed):
    def coherent():
        values = [observe(client, "/stem/cell", data=packed(point)) for point in points]
        if len({height for height, _ in values}) == 1:
            return values

    observations = wait_for(coherent, "same-height WASM signing Cell proofs")
    height = observations[0][0]
    records = [record for _, record in observations]
    assert all(record["cell"] and record["proof"] for record in records)
    root = args.output
    label = "intercell-send-0"
    request = json.loads((root / f"{label}-request.json").read_text())
    lock = json.loads((root / f"{label}-lock.json").read_text())
    digest = json.loads(run(args.binary, "wallet", "digest", "--network", client.network,
        "--transaction", root / f"{label}-draft.json", "--context", root / f"{label}-context.json",
        "--group-index", "0", "--lock", root / f"{label}-lock.json"))
    trust = json.loads(run(args.binary, "jolteon-trust", "--genesis", spec))
    return {"trust": trust, "signing": {**authenticate(client, height), "cells": records,
            "points": [packed(point) for point in points], "lock": lock, "request": request,
            "unsigned": unsigned, "signed": signed, "digest": digest}}


def finish_fixture(args, client, fixture, committed, message, proof_height, outbox):
    location = client.locations[committed["hash"]]
    height, execution = observe(client, "/stem/execution", height=location["height"],
                                data=hex_bytes(le(location["index"], 4)))
    assert execution["transaction"] == fixture["signing"]["signed"][2:]
    fixture["execution"] = {**authenticate(client, height), **execution, "id": committed["hash"]}
    fixture["outbox"] = {**authenticate(client, proof_height), "id": message["id"], **outbox}
    height, missing = observe(client, "/stem/cell", data=fixture["signing"]["points"][0])
    assert missing["cell"] is None
    fixture["spent"] = {**authenticate(client, height), **missing}
    height, revenue = observe(client, "/stem/vault-revenue")
    fixture["vault_revenue"] = {**authenticate(client, height), "accounting": revenue}
    file = args.output / "wasm-fixture.json"
    write_json(file, fixture)
    return file.name

"""Recompute the permit commitments and the matched return costs from the archived devnets."""

import hashlib
import json
import lzma
from pathlib import Path


def raw(value):
    return bytes.fromhex(value.removeprefix("0x"))


def digest(value):
    return hashlib.blake2b(value, digest_size=32, person=b"ckb-default-hash").digest()


def compact(cursor):
    value = 0
    for shift in range(0, 70, 7):
        byte = next(cursor)
        value |= (byte & 127) << shift
        if byte < 128:
            return value
    raise ValueError("invalid compact integer")


def execution(item):
    # Strand receipt begins with the optional protocol-result mask, fee and cycles.
    cursor = iter(raw(item["receipt"]))
    assert next(cursor) in (0, 1)
    fee = compact(cursor)
    return {"fee": fee, "cycles": compact(cursor), "bytes": len(raw(item["transaction"]))}


def summarize(run):
    settlement = run["settlement"]
    final = run["agreed"]
    assert len({r["member"] for r in run["replicas"]}) == 4
    assert all((r["height"], r["app_hash"]) == (final["height"], final["app_hash"]) for r in run["replicas"])
    assert settlement["original_ckb_cell"] == settlement["final_ckb_cell"]
    assert len(run["history"]["verified"]["transitions"]) == 4
    measurements = {"height": final["height"], "rejections": len(settlement["rejections"]),
                    "native": {k: execution(v) for k, v in settlement["execution"].items()}}
    if not run["permits"]:
        return measurements
    assert set(run["permits"]) == {"create", "gift", "return-gift", "export", "restore", "reexport"}
    for label, evidence in run["permits"].items():
        grant = settlement["permits"][label]
        req, signed = evidence["request"], evidence["transaction"]
        source = req["source"]
        expected = (b"SOP1" + bytes([int(label != "create")]) + raw(req["network"]) + raw(req["object"])
                    + raw(source["tx_hash"]) + int(source["index"], 16).to_bytes(4, "little")
                    + digest(raw(req["output"])))
        index = int(grant["out_point"]["index"], 16)
        assert signed["hash"] == grant["ckb"]["hash"] == grant["out_point"]["tx_hash"]
        assert len(expected) == 137 and expected == raw(grant["record"]) == raw(signed["json"]["outputs_data"][index])
        assert signed["json"]["outputs"][index]["type"] == grant["issuer"]
        assert {"out_point": grant["out_point"], "live_at_anchor": True} in evidence["references"]["references"]
        assert len({vote["signer"] for vote in evidence["certificate"]}) == 3
    restore = settlement["permits"]["restore"]
    deposit = settlement["deposited"]
    refs = run["permits"]["restore"]["references"]["references"]
    assert {"out_point": {"tx_hash": deposit["hash"], "index": "0x0"}, "live_at_anchor": False} in refs
    assert len(refs) == 2
    same_tx = restore["ckb"]["hash"] == deposit["hash"]
    measurements["extra_ckb_transactions"] = 6 - int(same_tx)
    if not same_tx:
        return measurements
    pair = restore["paired_return"]
    combined = run["permits"]["restore"]["transaction"]
    def inputs(signed):
        return {(v["previous_output"]["tx_hash"], v["previous_output"]["index"]) for v in signed["json"]["inputs"]}
    candidates = run["paired_candidates"]
    left, right = inputs(candidates["deposit"]), inputs(candidates["permit"])
    assert left.isdisjoint(right) and left | right == inputs(combined)
    separate_outputs = candidates["deposit"]["json"]["outputs"] + candidates["permit"]["json"]["outputs"]
    combined_outputs = combined["json"]["outputs"]
    assert len(separate_outputs) == len(combined_outputs)
    assert combined["json"]["outputs_data"] == (candidates["deposit"]["json"]["outputs_data"]
                                                + candidates["permit"]["json"]["outputs_data"])
    for old, new in zip(separate_outputs, combined_outputs):
        assert (old["lock"], old["type"]) == (new["lock"], new["type"])
    saved_fee = sum(int(x["capacity"], 16) for x in combined_outputs) - sum(
        int(x["capacity"], 16) for x in separate_outputs)
    assert saved_fee == 1_000_000
    for label, candidate in candidates.items():
        assert len(raw(candidate["molecule"])) == pair["separate"][label]["bytes"]
    assert len(raw(combined["molecule"])) == pair["combined"]["bytes"] == deposit["bytes"]
    assert pair["combined"]["cycles"] == deposit["cycles"]
    separate = {k: sum(row[k] for row in pair["separate"].values()) for k in ("cycles", "bytes")}
    measurements["return"] = {"separate": separate, "combined": pair["combined"], "fee_saved_shannons": saved_fee}
    return measurements


if __name__ == "__main__":
    with lzma.open(Path(__file__).with_name("devnets.json.xz"), "rt") as file:
        print(json.dumps({k: summarize(v) for k, v in json.load(file).items()}, indent=2))

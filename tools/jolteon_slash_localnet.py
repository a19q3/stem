"""Construct devnet double-vote witnesses using a real QC and the fixture signer."""

import json

from bridge_localnet import binary, ckb_hash, hex_bytes, le
from jolteon_localnet import varint
from localnet import run, write_json


def read_fields(raw):
    def read_number(offset):
        value, shift = 0, 0
        while offset < len(raw) and shift < 64:
            byte = raw[offset]
            offset += 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value, offset
            shift += 7
        raise ValueError("invalid protobuf field in fixture")

    fields, offset = {}, 0
    while offset < len(raw):
        tag, offset = read_number(offset)
        length, offset = read_number(offset)
        assert tag & 7 == 2 and offset + length <= len(raw) and tag >> 3 not in fields
        fields[tag >> 3] = raw[offset:offset + length]
        offset += length
    return fields


def evidence(args, relay, signer, epoch, sample_height, target):
    network = binary(relay.anchor["network"])
    genesis = binary(json.loads((args.output / "public-genesis-header.json").read_text()))
    genesis_id = ckb_hash(b"Stem Header\0" + network + genesis)
    if epoch == 0:
        anchor, anchor_id = genesis, genesis_id
        roster = binary(json.loads((relay.root / "initial-roster.json").read_text()))
        roster_path = frontier = siblings = b""
        generation = ckb_hash(b"Stem Jolteon committee\0" + roster[16:])
    else:
        raw = bytes.fromhex(json.loads((args.output / f"handoff-{epoch - 1}.json").read_text()))
        handoff = read_fields(raw)
        anchor, roster, roster_path = handoff[1], handoff[4], handoff[5]
        anchor_height = int.from_bytes(anchor[:8], "little")
        history = json.loads(run(args.binary, "checkpoint", "history", "--archive-db", relay.archive,
            "--index", relay.root / "history", "--network", relay.anchor["network"], "--height", str(target),
            "--header", str(anchor_height)))
        assert binary(history["header"]) == anchor
        frontier = binary(history["frontier"])
        siblings = b"".join(binary(value) for value in history["siblings"])
        anchor_id = ckb_hash(b"Stem Header\0" + network + anchor)
        generation = ckb_hash(b"Stem Jolteon successor committee\0" + anchor_id + roster[16:])
    response = relay.client.query("/stem/stake-epoch-end", data=hex_bytes(le(epoch, 8)), prove=True)
    end = json.loads(bytes.fromhex(response["value"]))
    end_height = end["end_height"] or 0
    assert end_height == 0 or end_height > sample_height
    assert relay.header(response["height"])[112:144] == relay.header(target)[112:144]

    finality = relay.client.finality(sample_height)
    assert finality["epoch"] == epoch and not finality["handoff"]
    proof = read_fields(bytes.fromhex(finality["proof"]))
    header, qc = proof[1], proof[2]
    assert int.from_bytes(header[:8], "little") == sample_height
    voters = [roster[i:i + 32] for i in range(16, len(roster), 40)]
    member = voters.index(signer.public_key().public_bytes_raw())
    bitmap_size = (len(voters) + 7) // 8
    bitmap = int.from_bytes(qc[48:48 + bitmap_size], "little")
    assert bitmap & (1 << member)
    rank = (bitmap & ((1 << member) - 1)).bit_count()
    signature_at = 48 + bitmap_size + rank * 64
    point, signature = qc[:48], qc[signature_at:signature_at + 64]
    binding = network + genesis_id + le(epoch, 8) + generation
    domain = b"Stem Jolteon vote\0" + binding
    assert ckb_hash(b"Stem Header\0" + network + header) == point[16:]
    signer.public_key().verify(signature, ckb_hash(domain + point))
    actual = point + le(member, 2) + signature + header

    def conflict(height, invalid_signature=False):
        alternative = le(height, 8) + header[8:80] + bytes([header[80] ^ 1]) + header[81:]
        point_alt = point[:16] + ckb_hash(b"Stem Header\0" + network + alternative)
        seal = signer.sign(ckb_hash(domain + point_alt))
        if invalid_signature:
            seal = bytes([seal[0] ^ 1]) + seal[1:]
        return point_alt + le(member, 2) + seal + alternative

    def payload(other):
        def field(tag, raw):
            return varint(tag << 3 | 2) + varint(len(raw)) + raw if raw else b""

        first, second = sorted((actual, other), key=lambda signed: signed[16:48])
        deadline = varint(6 << 3) + varint(end_height) if end_height else b""
        return (field(1, anchor) + field(2, roster) + field(3, roster_path) + field(4, frontier) + field(5, siblings)
                + deadline + field(7, binary(end["proof"]))
                + field(8, first) + field(9, second))

    good = payload(conflict(sample_height))
    forged = payload(conflict(sample_height, invalid_signature=True))
    late = payload(conflict(end_height or target + 1))
    penalty = ckb_hash(b"Stem Jolteon double vote\0" + binding + point[:8] + le(member, 2))
    write_json(args.output / "slash-evidence.json", {"witness": hex_bytes(good), "bad_signature": hex_bytes(forged),
        "late_vote": hex_bytes(late), "epoch": epoch, "height": sample_height, "end": end,
        "original_finality": finality, "penalty": hex_bytes(penalty)})
    return good, forged, late, penalty

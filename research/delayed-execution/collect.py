"""Package public native and CKB proofs from the two settlement fixtures."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path


def read(path):
    return json.loads(path.read_bytes())


def binary(value):
    return bytes.fromhex(value.removeprefix("0x"))


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def route(name, spec, genesis, checkpoint_root, report, proofs, headers):
    network = spec["network_id"]
    first = report["publications"][0]["height"]
    initial = read(checkpoint_root.parent / f"jolteon-checkpoints-checkpoint-{first}-signed.json")
    checkpoint_type = initial["json"]["outputs"][0]["type"]
    policy = binary(checkpoint_type["args"])
    genesis_id = hashlib.blake2b(b"Stem Header\0" + binary(network) + binary(genesis), digest_size=32,
                                person=b"ckb-default-hash").hexdigest()
    assert policy[32:64] == binary(network) and policy[96:128].hex() == genesis_id
    trust = {"network": network, "genesis": genesis_id,
             "roster": read(checkpoint_root / "initial-roster.json")}
    rows = []
    for (proof, terminal), header, publication in zip(proofs, headers, report["publications"], strict=True):
        height = header["height"]
        assert height == publication["height"]
        bundle = read(checkpoint_root / f"checkpoint-{height}-bundle.json")
        signed = read(checkpoint_root.parent / f"jolteon-checkpoints-checkpoint-{height}-signed.json")
        rows.append({"proof": proof, "terminal": terminal, "header": header, "bundle": bundle,
                     "publication": publication, "signed": signed})
    return {"name": name, "trust": trust, "checkpoint_type": checkpoint_type,
            "checkpoints": rows, "rejections": report["rejections"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("stem-run", "ckb-run", "epoch-run", "stem", "guest", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    native, ckb, epoch = args.stem_run, args.ckb_run, args.epoch_run
    relay = read(ckb / "ckb-relay-report.json")
    handoff = read(epoch / "jolteon-stake-report.json")
    plain = route(
        "standalone Stem and CKB relay", read(native / "genesis.json"), read(native / "public-genesis-header.json"),
        ckb / "jolteon-checkpoints", relay["publication"], [(read(native / "two-chain.json"), False)],
        [relay["stem_header"]],
    )
    plain["ckb_relay"] = relay["observations"]
    epochs = route(
        "CKB-backed committee transitions", read(epoch / "stem-genesis.json"), read(epoch / "public-genesis-header.json"),
        epoch / "jolteon-checkpoints", handoff["ckb_checkpoints"],
        [(read(epoch / f"handoff-{i}.json"), True) for i in range(2)] + [(read(epoch / "post-handoff.json"), False)],
        [*handoff["handoffs"], handoff["post_handoff_header"]],
    )
    with lzma.open(args.output, "xt") as out:
        json.dump({"stem_sha256": digest(args.stem), "guest_sha256": digest(args.guest), "routes": [plain, epochs]},
                  out, separators=(",", ":"))


if __name__ == "__main__":
    main()

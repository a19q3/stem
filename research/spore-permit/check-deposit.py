"""Exercise the standard deposit preflight with released-contract settlement fixtures."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess
import tempfile


def table(*fields):
    header_size = 4 * (len(fields) + 1)
    cursor = header_size
    offsets = []
    for field in fields:
        offsets.append(cursor)
        cursor += len(field)
    return b"".join(n.to_bytes(4, "little") for n in (cursor, *offsets)) + b"".join(fields)


def packed_bytes(data):
    return len(data).to_bytes(4, "little") + data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    archive = Path(__file__).resolve().parent / "upstream/collections/settlements.json.xz"
    with lzma.open(archive, "rt") as source, tempfile.TemporaryDirectory() as tmp:
        cases = json.load(source)
        root = Path(tmp)
        asset_file, cluster_file = root / "data.mol", root / "cluster.json"
        ordinary = cases["ordinary"]["settlement"]
        cluster = ordinary["cluster"]
        registered = cases["registered"]["settlement"]
        with lzma.open(archive.with_name("four-field-histories.json.xz"), "rt") as public:
            published = next(record["data"] for record in json.load(public)
                             if record["verified"]["transitions"][0]["output"]["data_hash"] == cluster["data_hash"])

        def run(asset, *flags):
            asset_file.write_bytes(bytes.fromhex(asset.removeprefix("0x")))
            return subprocess.run([args.stem.resolve(), "intercell", "spore-payload",
                                   "--original-type", "0x" + "01" * 32, "--recipient", "0x" + "02" * 32,
                                   "--data", asset_file, *flags], capture_output=True, text=True)

        def rejected(result, reason):
            if result.returncode == 0 or reason not in result.stderr:
                raise ValueError(f"expected {reason!r}, received {result.stderr!r}")

        def deposit(asset, collection_type, data):
            cluster_file.write_text(json.dumps({"type": collection_type, "data": data}))
            return run(asset, "--deposit", "--cluster", cluster_file)

        plain = cases["plain"]["settlement"]["native_content"]
        run(plain, "--deposit").check_returncode()
        member = ordinary["native_content"]
        first = deposit(member, cluster["type"], published)
        first.check_returncode()
        assert json.loads(first.stdout) == json.loads(run(member).stdout)

        populated = table(packed_bytes(b"Collection"), packed_bytes(b"description"), packed_bytes(b"\x01" * 32))
        rejected(deposit(member, cluster["type"], "0x" + populated.hex()), "collection-level Mutant")
        rejected(deposit(member, {**cluster["type"], "args": "0x" + "17" * 32}, published),
                 "member's released Cluster Type")
        rejected(deposit(member, cluster["type"], "0x1400000014000000"), "unsupported Cluster data layout")
        rejected(run(member, "--deposit"), "requires the live collection")
        rejected(run(registered["native_content"], "--deposit"), "outside the standard custody profile")
        run(registered["native_content"]).check_returncode()
        print("Verified ordinary deposit controls, all extension rejections and legacy payload encoding")


if __name__ == "__main__":
    main()

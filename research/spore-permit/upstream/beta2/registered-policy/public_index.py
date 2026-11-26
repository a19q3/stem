"""Find historical beta.2 Lua registrations and Spore outputs that actually reference them."""
import argparse
import json
import lzma
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parents[4] / "tools"))
from cluster_index import batch, pages
from bridge_localnet import binary, ckb_hash, hex_bytes, script_hash

ISSUER = "0x5ff1a403458b436ea4b2ceb72f1fa70a6507968493315b646f5302661cb68e57"
TYPE = "0xd2344a2eb941b70cd8a621d0e5d3a5e343a3716acd992d070d135f4f05734fdd"
SPORE = "0x685a60219309029d01310311dba953d67029170ca4848a4ff638e57002130a0d"


def outputs(rpc, code, kind, lower, upper):
    key = {"script": {"code_hash": code, "hash_type": kind, "args": "0x"}, "script_type": "type",
           "script_search_mode": "prefix", "filter": {"block_range": [hex(lower), hex(upper)]}}
    return [row for row in pages(rpc, "get_transactions", key) if row["io_type"] == "output"]


def records(rpc, entries):
    hashes = list(dict.fromkeys(row["tx_hash"] for row in entries))
    transactions = {}
    for start in range(0, len(hashes), 12):
        group = hashes[start:start+12]
        results = batch(rpc, [("get_transaction", [h]) for h in group])
        for h, result in zip(group, results, strict=True):
            if result["tx_status"]["status"] != "committed" or result["transaction"]["hash"] != h:
                raise ValueError("expected a committed creating transaction")
            transactions[h] = result["transaction"]
    for row in entries:
        tx = transactions[row["tx_hash"]]
        index = int(row["io_index"], 16)
        yield row, tx, tx["outputs"][index], binary(tx["outputs_data"][index])


def scan(rpc, height):
    tip, = batch(rpc, [("get_indexer_tip", [])])
    if not 0 <= height <= int(tip["block_number"], 16):
        raise ValueError("height must have been indexed")
    header, = batch(rpc, [("get_header_by_number", [hex(height)])])
    registrations = {}
    counts = {}
    for code, kind in [(ISSUER, "data1"), (TYPE, "type"), (ISSUER, "data"), (ISSUER, "data2")]:
        entries = outputs(rpc, code, kind, 0, height+1)
        counts[kind] = len(entries)
        for row, tx, cell, program in records(rpc, entries):
            ty = cell["type"]
            args = binary(ty["args"])
            if ty["code_hash"] != code or ty["hash_type"] != kind or len(args) < 32:
                raise ValueError("unexpected registration identity")
            registrations[tx["hash"], row["io_index"]] = {
                "height": int(row["block_number"], 16), "type": ty, "lock_hash": hex_bytes(script_hash(cell["lock"])),
                "data_hash": hex_bytes(ckb_hash(program)), "bytes": len(program),
            }

    windows = []
    for start in sorted(r["height"] for r in registrations.values()):
        end = min(start + 120, height + 1)
        if windows and start <= windows[-1][1]:
            windows[-1][1] = end
        else:
            windows.append([start, end])
    members = []
    for start, end in windows:
        for row, tx, cell, data in records(rpc, outputs(rpc, SPORE, "data1", start, end)):
            if cell["type"]["code_hash"] != SPORE or cell["type"]["hash_type"] != "data1":
                raise ValueError("unexpected Spore Type")
            mime_start = int.from_bytes(data[4:8], "little")
            mime_end = int.from_bytes(data[8:12], "little")
            mime = data[mime_start+4:mime_end]
            if int.from_bytes(data[mime_start:mime_start+4], "little") != len(mime):
                raise ValueError("malformed indexed Spore content type")
            ids = {v.strip() for field in mime.split(b";") if field.strip().startswith(b"mutant[]=")
                   for v in field.partition(b"=")[2].split(b",")}
            matches = []
            for dep in tx["cell_deps"]:
                point = dep["out_point"]
                key = (point["tx_hash"], point["index"])
                reg = registrations.get(key)
                if dep["dep_type"] == "code" and reg and reg["type"]["args"][2:66].encode() in ids:
                    matches.append(point)
            if matches:
                members.append({"point": {"tx_hash": tx["hash"], "index": row["io_index"]},
                                "height": int(row["block_number"], 16), "type": cell["type"],
                                "data_hash": hex_bytes(ckb_hash(data)), "mime": mime.decode(),
                                "registrations": matches})
    final, = batch(rpc, [("get_header_by_number", [hex(height)])])
    if final["hash"] != header["hash"]:
        raise ValueError("indexed chain changed during the lookup")
    return {"rpc": rpc, "height": height, "block_hash": header["hash"], "counts": counts, "windows": windows,
            "registrations": [{"point": {"tx_hash": h, "index": i}, **r} for (h, i), r in registrations.items()],
            "members": members}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc", default="https://testnet.ckb.dev")
    parser.add_argument("--height", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = scan(args.rpc, args.height)
    with lzma.open(args.output, "xt") as out:
        json.dump(result, out, separators=(",", ":"))
    print(result["counts"], len(result["members"]), "member outputs")

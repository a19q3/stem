"""Enumerate historical outputs for the two published beta.2 Cluster identities."""
import argparse
import hashlib
import json
import lzma
import subprocess
import time
from pathlib import Path

DATA = "0x0bbe768b519d8ea7b96d58f1182eb7e6ef96c541fbd9526975077ee09f049058"
TYPE = "0xc11dd38a124e544722bb6f473cb1224022f39919960dbddc364fc0e71ace3c5e"
PAGE = 512


def batch(rpc, calls):
    payload = [{"jsonrpc": "2.0", "method": method, "params": args, "id": i}
               for i, (method, args) in enumerate(calls)]
    raw = subprocess.check_output(["curl", "-fsS", "--retry", "2", "--max-time", "45", "-H",
                                   "Content-Type: application/json", "--data-binary", "@-", rpc],
                                  input=json.dumps(payload).encode())
    replies = sorted(json.loads(raw), key=lambda r: r["id"])
    if [r["id"] for r in replies] != list(range(len(calls))) or any(r.get("error") for r in replies):
        raise ValueError("missing or unsuccessful CKB RPC reply")
    return [r["result"] for r in replies]


def pages(rpc, method, query):
    cursor = None
    while True:
        params = [query, "asc", hex(PAGE)] + ([cursor] if cursor else [])
        page, = batch(rpc, [(method, params)])
        yield from page["objects"]
        if len(page["objects"]) < PAGE:
            return
        if cursor == page["last_cursor"]:
            raise ValueError("indexer cursor did not advance")
        cursor = page["last_cursor"]


def classify(raw):
    def number(pos):
        return int.from_bytes(raw[pos:pos + 4], "little")
    first = number(4)
    if number(0) != len(raw) or first < 12 or first % 4 or first > len(raw):
        return "unparsed", None
    ends = [number(i) for i in range(4, first, 4)] + [len(raw)]
    if any(b < a or b > len(raw) for a, b in zip(ends, ends[1:])):
        return "unparsed", None
    for start, end in zip(ends[:2], ends[1:3]):
        if end - start < 4 or number(start) != end - start - 4:
            return "unparsed", None
    if first == 12:
        return "two-field", None
    # Compatible tables may append fields after the optional Mutant ID.
    # Restrict the ID check to the third field and preserve the layout distinction.
    tail = raw[ends[2]:ends[3]]
    prefix = "extended-" if first > 16 else ""
    if not tail:
        return prefix + "absent", None
    if len(tail) == 36 and number(ends[2]) == 32:
        return prefix + "populated", "0x" + tail[4:].hex()
    return "unparsed", None


def scan(rpc, code, kind, tip):
    query = {"script": {"code_hash": code, "args": "0x", "hash_type": kind}, "script_type": "type",
             "script_search_mode": "prefix", "filter": {"block_range": ["0x0", hex(int(tip["block_number"], 16) + 1)]}}
    outputs = {}
    inputs = 0
    for item in pages(rpc, "get_transactions", query):
        if item["io_type"] == "input":
            inputs += 1
            continue
        key = (item["tx_hash"], int(item["io_index"], 16))
        if item["io_type"] != "output" or key in outputs:
            raise ValueError("unexpected CKB output index entry")
        outputs[key] = {"out_point": [key[0], key[1]], "height": int(item["block_number"], 16)}

    def add(key, cell, data):
        script = cell["type"]
        if script["code_hash"] != code or script["hash_type"] != kind or "shape" in outputs[key]:
            raise ValueError("indexer returned inconsistent Cluster identity or duplicate")
        raw = bytes.fromhex(data[2:])
        shape, mutant = classify(raw)
        outputs[key].update(args=script["args"], size=len(raw), shape=shape,
                            data_hash="0x" + hashlib.blake2b(raw, digest_size=32, person=b"ckb-default-hash").hexdigest())
        if mutant is not None:
            outputs[key]["mutant_id"] = mutant

    live_query = {**query, "with_data": True, "filter": {**query["filter"], "output_data_len_range": ["0x0", "0x10001"]}}
    for cell in pages(rpc, "get_cells", live_query):
        p = cell["out_point"]
        add((p["tx_hash"], int(p["index"], 16)), cell["output"], cell["output_data"])
    compact_live = sum("shape" in row for row in outputs.values())
    unresolved = {}
    for (tx_hash, index), row in outputs.items():
        if "shape" not in row:
            unresolved.setdefault(tx_hash, []).append(index)
    hashes = list(unresolved)
    for i in range(0, len(hashes), 16):
        group = hashes[i:i + 16]
        transactions = batch(rpc, [("get_transaction", [h]) for h in group])
        for h, record in zip(group, transactions, strict=True):
            tx = record["transaction"]
            if tx["hash"] != h or record["tx_status"]["status"] != "committed":
                raise ValueError("expected a committed creating transaction")
            for index in unresolved[h]:
                add((h, index), tx["outputs"][index], tx["outputs_data"][index])
        time.sleep(0.25)
    return {"query": query, "live_query": live_query, "indexed_inputs": inputs, "compact_live_outputs": compact_live,
            "history_transactions_fetched": len(hashes), "rows": list(outputs.values())}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc", default="https://testnet.ckb.dev")
    parser.add_argument("--height", type=lambda n: int(n, 0))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    tip, = batch(args.rpc, [("get_indexer_tip", [])])
    if args.height is not None:
        if not 0 <= args.height <= int(tip["block_number"], 16):
            raise ValueError("requested height outside indexed chain")
        header, = batch(args.rpc, [("get_header_by_number", [hex(args.height)])])
        tip = {"block_hash": header["hash"], "block_number": hex(args.height)}
    records = [scan(args.rpc, code, kind, tip) for code, kind in ((DATA, "data1"), (TYPE, "type"))]
    header, = batch(args.rpc, [("get_header_by_number", [tip["block_number"]])])
    if header["hash"] != tip["block_hash"]:
        raise ValueError("indexed chain changed during the lookup")
    with lzma.open(args.output, "wt") as out:
        json.dump({"rpc": args.rpc, "tip": tip, "page_size": PAGE, "datasets": records}, out, separators=(",", ":"))
    for record in records:
        print(record["query"]["script"]["hash_type"], len(record["rows"]), "outputs",
              sum("mutant_id" in r for r in record["rows"]), "populated")


if __name__ == "__main__":
    main()

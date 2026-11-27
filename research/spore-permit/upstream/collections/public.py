"""Build CKB birth proofs for extended Cluster records from the bounded public scan."""
import argparse
import json
import lzma
from pathlib import Path
import subprocess
import tempfile

from migrations import HERE
from cluster_index import batch
from replay import binary, ckb_hash, hex_bytes, script_hash, verify


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--index", type=Path, default=HERE / "testnet.json.xz")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--stem", type=Path, required=True)
    args = parser.parse_args()
    with lzma.open(args.index, "rt") as source:
        index = json.load(source)
    records = []
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        for row in index["datasets"][0]["rows"]:
            if not row["shape"].startswith("extended-"):
                continue
            tx_hash, position = row["out_point"]
            header, tx = batch(index["rpc"], [("get_header_by_number", [hex(row["height"])]),
                                              ("get_transaction", [tx_hash])])
            assert tx["tx_status"]["block_hash"] == header["hash"]
            cell = tx["transaction"]["outputs"][position]["type"]
            raw = tx["transaction"]["outputs_data"][position]
            request = {"anchor": header["hash"], "type_hash": hex_bytes(script_hash(cell)),
                       "origin": {"tx_hash": tx_hash, "index": hex(position)}, "end_transaction": tx_hash,
                       "required_dependencies": [{"data_hash": cell["code_hash"]}]}
            req = work / "request.json"
            req.write_text(json.dumps(request))
            stem = args.stem.resolve()
            proof = json.loads(subprocess.check_output([stem, "ckb-wallet", "prove-type-history",
                               "--request", req, "--l1-rpc", index["rpc"]]))
            record = {"request": request, "proof": proof, "type": cell, "data": raw}
            result = verify(stem, work, record, request)
            result.check_returncode()
            record["verified"] = json.loads(result.stdout)
            assert record["verified"]["transitions"][0]["output"]["data_hash"] == row["data_hash"] == hex_bytes(ckb_hash(binary(raw)))
            records.append(record)
    with lzma.open(args.output, "xt") as out:
        json.dump(records, out, separators=(",", ":"))
    print(len(records), "public birth proofs")


if __name__ == "__main__":
    main()

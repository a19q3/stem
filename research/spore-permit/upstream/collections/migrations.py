"""Check the earlier published Cluster deployments on both CKB chains."""
import argparse
from collections import Counter
from hashlib import blake2b
import json
import lzma
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "beta2"))
from cluster_index import batch, scan

DEPLOYMENTS = {
    "mainnet": {"file": "2024-01-23-0.2.2-beta.1.json",
                "tx": "0xe464b7fb9311c5e2820e61c99afc615d6b98bdefbe318c34868c010cbd0dc938"},
    "testnet": {"file": "2024-01-19-0.2.1.json",
                "tx": "0xfbceb70b2e683ef3a97865bb88e082e3e5366ee195a9c826e3c07d1026792fcd"},
}
DATA = "0x7366a61534fa7c7e6225ecc0d828ea3b5366adec2b58206f2ee84995fe030075"
TESTNET_TYPE = "0x95cb51b763d4a73ac695f05fefdf3bcbe189cc6e0e71497360d2df45915c53c5"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("network", choices=DEPLOYMENTS)
    parser.add_argument("--rpc")
    parser.add_argument("--height", type=int)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rpc = args.rpc or f"https://{args.network}.ckb.dev"
    deployment = DEPLOYMENTS[args.network]
    tip, deployed = batch(rpc, [("get_indexer_tip", []), ("get_transaction", [deployment["tx"]])])
    if args.height is not None:
        assert 0 <= args.height <= int(tip["block_number"], 16)
        header, = batch(rpc, [("get_header_by_number", [hex(args.height)])])
        tip = {"block_number": header["number"], "block_hash": header["hash"]}
    assert deployed["tx_status"]["status"] == "committed" and deployed["transaction"]["hash"] == deployment["tx"]
    data = bytes.fromhex(deployed["transaction"]["outputs_data"][0][2:])
    assert "0x" + blake2b(data, digest_size=32, person=b"ckb-default-hash").hexdigest() == DATA
    identities = [(DATA, "data1")]
    if args.network == "testnet":
        from replay import hex_bytes, script_hash
        assert hex_bytes(script_hash(deployed["transaction"]["outputs"][0]["type"])) == TESTNET_TYPE
        identities.append((TESTNET_TYPE, "type"))
    else:
        assert deployed["transaction"]["outputs"][0]["type"] is None
    datasets = [scan(rpc, code, kind, tip) for code, kind in identities]
    header, = batch(rpc, [("get_header_by_number", [tip["block_number"]])])
    assert header["hash"] == tip["block_hash"]
    with lzma.open(args.output, "xt") as out:
        json.dump({"network": args.network, "rpc": rpc, "tip": tip, "deployment": deployment,
                   "deployment_block": deployed["tx_status"]["block_hash"], "code_bytes": len(data),
                   "datasets": datasets}, out, separators=(",", ":"))
    for dataset in datasets:
        print(args.network, dataset["query"]["script"]["hash_type"], Counter(r["shape"] for r in dataset["rows"]))


if __name__ == "__main__":
    main()

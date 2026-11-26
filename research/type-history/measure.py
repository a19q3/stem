"""Measure fresh offline verifier processes against the archived portable witnesses."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stem", required=True, type=Path)
    parser.add_argument("--archive", type=Path, default=Path(__file__).with_name("histories.json.xz"))
    args = parser.parse_args()
    stem = args.stem.resolve()
    with stem.open("rb") as stream:
        binary_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    with lzma.open(args.archive, "rt") as stream:
        cases = json.load(stream)
    result = {}
    with tempfile.TemporaryDirectory() as tmp:
        for label, case in cases.items():
            request = Path(tmp) / "request.json"
            proof = Path(tmp) / "proof.json"
            request.write_text(json.dumps(case["request"], separators=(",", ":")))
            proof.write_text(json.dumps(case["proof"], separators=(",", ":")))
            elapsed = []
            for _ in range(11):
                start = time.perf_counter_ns()
                answer = subprocess.check_output([stem, "ckb-wallet", "verify-type-history", "--request", request,
                                                  "--proof", proof])
                elapsed.append((time.perf_counter_ns() - start) // 1000)
                assert json.loads(answer) == case["verified"]
            bundle = case["proof"]
            result[label] = {
                "json_bytes": proof.stat().st_size,
                "reference_raw_bytes": sum((len(raw) - 2) // 2 for raw in bundle["references"]),
                "witnessed_tx_bytes": sum((len(s["transaction"]) - 2) // 2 for s in bundle["steps"]),
                "packed_header_bytes": sum((len(raw) - 2) // 2 for raw in bundle["headers"]),
                "median_us": statistics.median(elapsed),
                "process_wall_us": elapsed,
            }
    print(json.dumps({"binary_sha256": binary_sha256, "cases": result}, indent=2))


if __name__ == "__main__":
    main()

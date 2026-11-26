"""Compare complete native Lineage replays across binaries with identical signed inputs."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "storage"))
from summarize import summarize


def compare(paths):
    cohorts = {}
    rows = []
    for path in paths:
        metrics = summarize(path)
        with lzma.open(path, "rt") as source:
            archive = json.load(source)
        report = archive["report.json"]
        ids = sorted(txid for block in report["blocks"] for txid in block["ids"])
        assert metrics["admitted"] == metrics["visible"] == metrics["planned"] == len(ids)
        check = {
            "seconds": metrics["seconds"], "genesis": metrics["genesis_sha256"],
            "signed_workload": metrics["workload_sha256"],
            "confirmed_set": hashlib.sha256("".join(ids).encode("ascii")).hexdigest(),
            "cycles": metrics["lineage"]["cycles"], "fees": metrics["lineage"]["fee"],
            "head_updates": metrics["lineage"]["head_updates"],
        }
        rate = report["load"]["rate"]
        assert cohorts.setdefault(rate, check) == check, f"mismatched replay at {path}"
        rows.append({
            "file": path.name, "binary_sha256": archive["binary_sha256"], "rate": rate,
            **{key: metrics[key] for key in (
                "visible", "visible_tps", "candidate_verify_mean_us", "p99_common_us",
                "cpu_seconds", "peak_sampled_node_rss_kib", "observer_errors",
            )},
        })
    return {"cohorts_by_offer_rate": cohorts, "runs": rows}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archives", nargs="+", type=Path)
    print(json.dumps(compare(parser.parse_args().archives), indent=2))

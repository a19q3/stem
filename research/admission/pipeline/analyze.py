"""Compare two forward/reverse network pairs and account for stale admission results."""
import argparse
from importlib.util import module_from_spec, spec_from_file_location
import json
import lzma
from pathlib import Path

HERE = Path(__file__).resolve().parent
REFERENCE = HERE.parents[1] / "storage/receipt-crossover/analyze.py"


def read(path):
    return json.loads(path.read_bytes())


def trial(path, comparison):
    result = comparison.attribution.analyze(path)
    meta = read(path / "counters.json")
    with lzma.open(path / "payments.json.xz", "rt") as source:
        payments = json.load(source)
    with lzma.open(path / "cpu.json.xz", "rt") as source:
        cpu = json.load(source)
    starts = payments["driver_clock"]["start"]
    start = starts["unix_us"] - (starts["before_us"] + starts["after_us"]) // 2
    end = start + 1_000_000 * result["seconds"]
    if cpu["samples"][0][1] <= start and cpu["samples"][-1][0] >= end:
        result["offer_cpu"] = comparison.sessions.cpu_bounds(cpu, start, end)["validators"]
    else:
        result["offer_cpu"] = None
    queue_us = sum(after["queue"]["elapsed_us"] - before["queue"]["elapsed_us"]
                   for before, after in zip(meta["before"], meta["after"], strict=True))
    result["client_queue_us_per_write"] = queue_us / result["summary"]["offered"]
    result["write_to_common_p99_us"] = payments["summary"]["latency_us"]["p99"]
    result["node_sha256"] = meta["node_sha256"]
    result["configuration"] = meta["preparation"]
    return result


def analyze(root):
    spec = spec_from_file_location("payment_analysis", REFERENCE)
    comparison = module_from_spec(spec)
    spec.loader.exec_module(comparison)
    names = ("a1", "b1", "b2", "a2")
    results = {name: trial(root / name, comparison) for name in names}
    for key in ("workload", "genesis", "configuration"):
        assert len({json.dumps(row[key], sort_keys=True) for row in results.values()}) == 1
    ref, candidate = (results[k]["node_sha256"] for k in ("a1", "b1"))
    assert ref != candidate and results["a2"]["node_sha256"] == ref and results["b2"]["node_sha256"] == candidate

    fields = ("dispatched", "coalesced", "full", "snapshot_misses", "rechecked")
    counters = {}
    for name in names:
        path = root / name
        pipeline = read(path / "pipeline.json")
        before, after = pipeline["before"], pipeline["after"]
        if name.startswith("a"):
            assert before == after == [None] * 4
            counters[name] = None
            continue
        assert len(before) == len(after) == 4
        deltas = [{field: end[field] - start[field] for field in fields}
                  for start, end in zip(before, after, strict=True)]
        assert all(0 <= node["pending_replies"] <= 128 and 0 <= node["pending_bytes"] <= 4 * 1024 * 1024
                   for node in before + after)
        assert all(min(row.values()) >= 0 for row in deltas)
        counters[name] = {field: sum(row[field] for row in deltas) for field in fields}
    pairs = [comparison.compare(root, results, a, b, ("serial", "background"))
             for a, b in (("a1", "b1"), ("a2", "b2"))]
    return {"trials": results, "pairs": pairs, "admission": counters}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trials", nargs="?", type=Path, default=HERE)
    args = parser.parse_args()
    print(json.dumps(analyze(args.trials), indent=2))

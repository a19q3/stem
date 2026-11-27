"""Check matched Jolteon runs and summarize control queue and candidate work."""
import argparse
from importlib.util import module_from_spec, spec_from_file_location
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
REFERENCE = HERE.parent / "storage/receipt-crossover/analyze.py"
RUNS = ("a1", "b1", "b2", "a2")


def read(path):
    return json.loads(path.read_bytes())


def controls(path):
    metrics = read(path / "counters.json")
    views = read(path / "views.json")
    jobs = {}
    control_buckets = [0] * 6
    prepare_buckets = [0] * 6
    control_us = prepare_us = local_timeouts = certified_timeout_markers = 0
    previous, following = metrics["before"], metrics["after"]
    for i, (start, end) in enumerate(zip(previous, following, strict=True)):
        earlier, later = (views[side][i] for side in ("before", "after"))
        assert start["member"] == end["member"] == earlier["member"] == later["member"] == i
        assert earlier["epoch"] == later["epoch"]
        old, new = start["execution"], end["execution"]
        local_timeouts += new["local_timeouts"] - old["local_timeouts"]
        certified_timeout_markers += len(set(later["timeout_views"]) - set(earlier["timeout_views"]))
        control_us += new["control_queue"]["elapsed_us"] - old["control_queue"]["elapsed_us"]
        prepare_us += new["candidate_verify"]["elapsed_us"] - old["candidate_verify"]["elapsed_us"]
        for field, total in (("control_queue", control_buckets), ("candidate_verify", prepare_buckets)):
            for j, (a, b) in enumerate(zip(old[field]["buckets"], new[field]["buckets"], strict=True)):
                assert b >= a
                total[j] += b - a
        for snapshot in (old, new):
            work = snapshot["candidate_jobs"]
            observed_in_flight = work["started"] - work["applied"] - work["invalid"] - work["stale"]
            assert 0 <= observed_in_flight <= 1
        for name, total in new["candidate_jobs"].items():
            delta = total - old["candidate_jobs"][name]
            assert delta >= 0
            jobs[name] = jobs.get(name, 0) + delta
    received, prepared = sum(control_buckets), sum(prepare_buckets)
    assert received > 0 and prepared > 0
    return {
        "local_timeouts": local_timeouts, "observed_timeout_markers": certified_timeout_markers,
        "control_messages": received, "control_queue_buckets": control_buckets,
        "control_queue_mean_us": control_us / received,
        "control_queue_over_10ms": sum(control_buckets[2:]),
        "preparation_samples": prepared, "preparation_buckets": prepare_buckets,
        "preparation_mean_us": prepare_us / prepared,
        "candidate_jobs": jobs,
    }


def analyze(root):
    spec = spec_from_file_location("payment_analysis", REFERENCE)
    comparison = module_from_spec(spec)
    spec.loader.exec_module(comparison)
    results = {name: comparison.analyze(root / name) for name in RUNS}
    for key in ("workload", "genesis", "configuration"):
        assert len({json.dumps(row[key], sort_keys=True) for row in results.values()}) == 1
    reference, candidate = (results[k]["node_sha256"] for k in ("a1", "b1"))
    assert reference != candidate
    assert results["a2"]["node_sha256"] == reference and results["b2"]["node_sha256"] == candidate
    pairs = [comparison.compare(root, results, a, b, ("sync", "worker"))
             for a, b in (("a1", "b1"), ("a2", "b2"))]
    return {"trials": results, "pairs": pairs, "control": {name: controls(root / name) for name in RUNS}}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trials", nargs="?", type=Path, default=HERE)
    args = parser.parse_args()
    print(json.dumps(analyze(args.trials), indent=2))

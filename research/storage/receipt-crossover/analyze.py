"""Compare four uninstrumented payment trials in reference/candidate/candidate/reference order."""
import argparse
from importlib.util import module_from_spec, spec_from_file_location
import json
import lzma
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent


def load_reader(folder):
    path = HERE.parent / folder
    sys.path.insert(0, str(path))
    spec = spec_from_file_location(folder, path / "analyze.py")
    reader = module_from_spec(spec)
    spec.loader.exec_module(reader)
    return reader


attribution = load_reader("queue-attribution")
sessions = load_reader("submission-sessions")


def analyze(path):
    result = attribution.analyze(path)
    with lzma.open(path / "payments.json.xz", "rt") as archive:
        trial = json.load(archive)
    with lzma.open(path / "cpu.json.xz", "rt") as archive:
        cpu = json.load(archive)
    assert not result.pop("nodes") and not result.pop("aggregate")
    meta = json.loads((path / "counters.json").read_bytes())
    clock = trial["driver_clock"]["start"]
    start = clock["unix_us"] - (clock["before_us"] + clock["after_us"]) // 2
    times = trial["delivery_times"]
    result["writes_by_replica"] = [trial["delivery_routes"].count(i) for i in range(4)]
    latencies = sorted(block["seen_us"] - times[index][0] for block in trial["blocks"]
                       for index in block["positions"] if times[index][1] is not None)
    assert latencies and latencies[0] >= 0
    result["write_to_common_p99_us"] = latencies[(99 * len(latencies) + 99) // 100 - 1]
    for name, seconds in (("offer", result["seconds"]), ("offer_and_drain", result["seconds"] + trial["load"]["drain"])):
        result["cpu_" + name] = sessions.cpu_bounds(cpu, start, start + seconds * 1_000_000)
    result["node_sha256"] = meta["node_sha256"]
    result["configuration"] = meta["preparation"]
    return result


def payment_observations(directory):
    with lzma.open(directory / "payments.json.xz", "rt") as archive:
        trial = json.load(archive)
    times = trial["delivery_times"]
    durations = {index: block["seen_us"] - times[index][0] for block in trial["blocks"]
                 for index in block["positions"] if times[index][1] is not None}
    return durations, trial["delivery_routes"]


def compare(directory, results, reference_run, candidate_run, labels=("serial", "parallel")):
    a, b = results[reference_run], results[candidate_run]
    cumulative = max_cumulative = 0
    for x, y in zip(a["written_per_second"], b["written_per_second"], strict=True):
        cumulative += y - x
        max_cumulative = max(max_cumulative, abs(cumulative))
    reference, reference_routes = payment_observations(directory / reference_run)
    candidate, candidate_routes = payment_observations(directory / candidate_run)
    matched = reference.keys() & candidate.keys()
    differences = sorted(candidate[i] - reference[i] for i in matched)
    return {labels[0]: reference_run, labels[1]: candidate_run, "window_write_difference": cumulative,
            "max_cumulative_write_difference_at_second_boundary": max_cumulative,
            "common_tps_difference": b["window_tps"] - a["window_tps"],
            "write_to_common_p99_difference_us": b["write_to_common_p99_us"] - a["write_to_common_p99_us"],
            "acknowledged_and_certified_in_both": len(matched),
            "matched_with_same_receiving_replica": sum(reference_routes[i] == candidate_routes[i] for i in matched),
            f"matched_faster_in_{labels[1]}": sum(t < 0 for t in differences),
            "matched_latency_difference_us": {f"p{p}": differences[(p * len(differences) + 99) // 100 - 1]
                                             for p in (10, 50, 90)}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", nargs="?", type=Path, default=HERE)
    parser.add_argument("--labels", nargs=2, default=("serial", "parallel"), metavar=("REFERENCE", "CANDIDATE"))
    args = parser.parse_args()
    results = {name: analyze(args.directory / name) for name in ("a1", "b1", "b2", "a2")}
    for key in ("workload", "genesis", "configuration"):
        assert all(row[key] == results["a1"][key] for row in results.values())
    assert results["a1"]["node_sha256"] == results["a2"]["node_sha256"]
    assert results["b1"]["node_sha256"] == results["b2"]["node_sha256"] != results["a1"]["node_sha256"]
    pairs = [compare(args.directory, results, a, b, args.labels) for a, b in (("a1", "b1"), ("a2", "b2"))]
    print(json.dumps({"trials": results, "pairs": pairs}, indent=2))


if __name__ == "__main__":
    main()

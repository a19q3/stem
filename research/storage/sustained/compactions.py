"""Join each RocksDB compaction's actual input and output SSTs to their encoded namespaces."""

import argparse
from collections import Counter, defaultdict
import json
import lzma
from pathlib import Path


def analyze(archive, attribution):
    with lzma.open(archive, "rt") as stream:
        run = json.load(stream)
    sst = run["sst"]
    captured = json.loads(attribution.read_bytes())
    assert captured["recorded_bytes"] == captured["decoded_bytes"], "complete capture required"
    files = captured["files"]
    output_files = defaultdict(list)
    started, finished = {}, {}
    for log, entries in sst["logs"].items():
        node = Path(log).parts[0]
        for item in entries:
            if item["kind"] == "compaction":
                output_files[node, item["job"]].append(f"{node}/{item['file_number']:06}.sst")
    for log, entries in sst["compaction_events"].items():
        node = Path(log).parts[0]
        for event in entries:
            bucket = started if event["event"] == "compaction_started" else finished
            key = node, event["job"]
            assert key not in bucket
            bucket[key] = event
    assert set(started) == set(finished), "incomplete compaction job"
    assert set(output_files) <= set(started)

    def encoded(names):
        counts = Counter()
        for name in names:
            source = files[name]
            counts.update({ns: values["bytes"] for ns, values in source["namespaces"].items()})
            assert sum(v["bytes"] for v in source["namespaces"].values()) == source["bytes"]
        return counts

    in_namespaces, out_namespaces = Counter(), Counter()
    levels_in, levels_out = Counter(), Counter()
    jobs = []
    for key, start in sorted(started.items()):
        node, number = key
        end = finished[key]
        inputs = {int(k[7:]): [f"{node}/{n:06}.sst" for n in ids]
                  for k, ids in start.items() if k.startswith("files_L")}
        incoming = [name for names in inputs.values() for name in names]
        outgoing = output_files[key]
        assert len(incoming) == len(set(incoming))
        assert len(outgoing) == end["num_output_files"]
        before, after = encoded(incoming), encoded(outgoing)
        assert sum(before.values()) == start["input_data_size"]
        assert sum(after.values()) == end["total_output_size"]
        in_namespaces.update(before)
        out_namespaces.update(after)
        for level, names in inputs.items():
            levels_in[level] += sum(files[n]["bytes"] for n in names)
        levels_out[end["output_level"]] += sum(after.values())
        jobs.append(dict(node=node, job=number, input_files=inputs, output_files=outgoing,
                         input_entries=end["num_input_records"], output_entries=end["num_output_records"],
                         reasons=start["compaction_reason"], input_bytes=before, output_bytes=after))
    flush = captured["encoded_sst_bytes_by_job_and_namespace"]["flush"]
    assert out_namespaces == Counter(captured["encoded_sst_bytes_by_job_and_namespace"]["compaction"])
    return {
        "archive": archive.name, "jobs": jobs,
        "input_by_level": levels_in, "output_by_level": levels_out,
        "compaction_input_by_namespace": in_namespaces, "compaction_output_by_namespace": out_namespaces,
        "sst_bytes_written_per_flush_byte": {
            ns: (amount + out_namespaces[ns]) / amount for ns, amount in flush.items() if amount
        },
        "input_entries": sum(j["input_entries"] for j in jobs),
        "output_entries": sum(j["output_entries"] for j in jobs),
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("sst", type=Path)
    args = parser.parse_args()
    print(json.dumps(analyze(args.archive, args.sst), indent=2))

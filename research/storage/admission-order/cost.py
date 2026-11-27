"""Measure the two mempool test binaries in forward and reverse order on a signed corpus."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(executable, env):
    with tempfile.TemporaryFile() as log:
        with subprocess.Popen([executable, "--exact", "fill_and_drain", "--nocapture"], env=env,
                              stdout=log, stderr=subprocess.STDOUT) as child:
            _, status, usage = os.wait4(child.pid, 0)
            child.returncode = os.waitstatus_to_exitcode(status)
            log.seek(0)
            output = log.read().decode()
            if child.returncode:
                raise RuntimeError(output)
    columns = ("round", "wire_bytes", "fill_us", "scan_us", "remove_us", "filled_rss_kib",
               "filled_peak_kib", "empty_rss_kib", "empty_peak_kib")
    samples = [dict(zip(columns, map(int, line.split()[2:]), strict=True))
               for line in output.splitlines() if line.startswith("POOL round ")]
    assert [row["round"] for row in samples] == [0, 1]
    return {"sha256": digest(executable), "peak_rss_kib": usage.ru_maxrss, "rounds": samples}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("reference", "candidate", "workload", "output"):
        parser.add_argument("--" + option, required=True, type=Path)
    args = parser.parse_args()
    env = {**os.environ, "STEM_SIGNED_PAYMENTS": str(args.workload.resolve())}
    measurements = [{"variant": name, **run(getattr(args, name).resolve(), env)}
                    for name in ("reference", "candidate", "candidate", "reference")]
    result = {"workload_sha256": digest(args.workload), "cpus": sorted(os.sched_getaffinity(0)), "runs": measurements}
    with args.output.open("x") as out:
        json.dump(result, out, indent=2)
        out.write("\n")


if __name__ == "__main__":
    main()

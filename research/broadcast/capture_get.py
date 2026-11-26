"""Observe native payload retrieval during ordinary offline/rejoin fixtures."""

import argparse
import collections
import hashlib
import json
import lzma
from pathlib import Path
import subprocess
import sys
import tempfile


def results(events):
    counts = collections.Counter(row["action"] for row in events)
    groups = collections.defaultdict(list)
    for row in events:
        if row["action"] == "hit":
            groups[row["epoch"], row["peer"], row["point"]].append(row)
    repeated = []
    for key, rows in groups.items():
        rows.sort(key=lambda e: e["at_us"])
        assert len({r["bytes"] for r in rows}) == 1
        if len(rows) > 1:
            repeated.append(
                dict(
                    epoch=key[0],
                    requester=key[1],
                    point=key[2],
                    hits=len(rows),
                    peers=len({r["node"] for r in rows}),
                    bytes_after_first=sum(r["bytes"] for r in rows[1:]),
                )
            )
    return dict(
        events=dict(counts),
        points_with_reply=len(groups),
        reply_bytes=sum(e["bytes"] for e in events if e["action"] == "hit"),
        repeat_groups=repeated,
    )


def main():
    p = argparse.ArgumentParser()
    for arg in ("binary", "lock", "scratch", "output"):
        p.add_argument("--" + arg, required=True, type=Path)
    p.add_argument("--port", type=int, default=17200)
    args = p.parse_args()
    assert not args.output.exists()
    args.scratch.mkdir(parents=True, exist_ok=True)
    runs = []
    for n in [4, 16]:
        with tempfile.TemporaryDirectory(dir=args.scratch) as scratch:
            f = Path(scratch)
            cmd = [
                sys.executable,
                "tools/jolteon_localnet.py",
                "--binary",
                args.binary,
                "--lock",
                args.lock,
                "--output",
                f / "run",
                "--port",
                str(args.port + n * 10),
                "--validators",
                str(n),
                "--transactions",
                str(3 * n),
                "--offline",
                str(1 if n == 4 else 2),
                "--offline-blocks",
                "12",
                "--offered",
                "12",
                "--recovery-seconds",
                "100",
            ]
            outcome = subprocess.run(cmd, capture_output=True, text=True)
            events = []
            for path in sorted((f / "run").glob("node*.log")):
                for line in path.read_text().splitlines():
                    if line.startswith("STEM_GET "):
                        _, at, node, epoch, action, peer, point, size = line.split()
                        events.append(
                            dict(
                                at_us=int(at),
                                node=int(node),
                                epoch=int(epoch),
                                action=action,
                                peer=int(peer),
                                point=point,
                                bytes=int(size),
                                log=path.name,
                            )
                        )
            row = dict(
                replicas=n,
                exit=outcome.returncode,
                log=outcome.stdout + outcome.stderr,
                events=sorted(events, key=lambda e: e["at_us"]),
                summary=results(events),
            )
            report = f / "run/jolteon-report.json"
            if report.exists():
                row["fixture"] = json.loads(report.read_text())
                row["signed_workload_sha256"] = hashlib.sha256(
                    (f / "run/signed-workload.json").read_bytes()
                ).hexdigest()
            runs.append(row)
            with lzma.open(args.output, "wt") as out:
                json.dump(
                    dict(
                        binary_sha256=hashlib.sha256(
                            args.binary.read_bytes()
                        ).hexdigest(),
                        runs=runs,
                    ),
                    out,
                )
            assert outcome.returncode == 0, outcome.stderr
            print(n, json.dumps(row["summary"], separators=(",", ":")), flush=True)


if __name__ == "__main__":
    main()

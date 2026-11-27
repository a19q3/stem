"""Check partial and adjacent phases, overlapping client waits and histogram alignment."""
import hashlib
import lzma
from pathlib import Path
import random
import tempfile
import unittest

from analyze import RECORD, intersect, spans


class QueueAttribution(unittest.TestCase):
    def test_matches_per_nanosecond_reference(self):
        rng = random.Random(49277)
        names = ["candidate_verify", "proposal", "commit", "client_check", "relay_check"]
        phases = {name: [] for name in names}
        clock = ["other"] * 1024
        t = 0
        while t < len(clock):
            name = rng.choice(names)
            end = min(t + rng.randint(1, 32), len(clock))
            phases[name].append((t, end))
            clock[t:end] = [name] * (end - t)
            t = end + rng.randrange(6)
        phases["queue"] = [tuple(sorted(rng.choices(range(len(clock)), k=2))) for _ in range(200)]
        phases["queue"] += [(start, end) for name in names for start, end in phases[name]]
        actual = intersect(phases)
        for name in [*names, "other"]:
            self.assertEqual(actual["overlap_ns"][name],
                             sum(clock[a:b].count(name) for a, b in phases["queue"]))
            self.assertEqual(actual["arrival_phase"][name],
                             sum(clock[a] == name for a, b in phases["queue"]))

    def test_long_wait_that_spans_adjacent_phases(self):
        phases = {"candidate_verify": [(0, 50_000_000)], "proposal": [(50_000_000, 100_000_000)],
                  "queue": [(0, 100_000_000), (25_000_000, 50_000_000), (100_000_000, 100_000_000)]}
        actual = intersect(phases)
        self.assertEqual(actual["waits_at_least_100ms"], 1)
        self.assertEqual(actual["long_wait_overlap_ns"],
                         {"candidate_verify": 50_000_000, "proposal": 50_000_000, "other": 0})

    def test_counter_slices_exclude_prefunding(self):
        metric = lambda count, elapsed: {"buckets": [count], "elapsed_us": elapsed}
        before = {"queue": metric(1, 1), "execution": {"candidate_verify": metric(0, 0)},
                  "checks": 0, "verification_us": 0}
        after = {"queue": metric(2, 4), "execution": {"candidate_verify": metric(1, 2)},
                 "checks": 0, "verification_us": 0}
        data = b"".join(RECORD.pack(*row) for row in [
            (b"Q", 0, 1100), (b"V", 2000, 4800), (b"Q", 1800, 5000), (b"Q", 6000, 7100)])
        sha = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "node.trace.xz"
            path.write_bytes(lzma.compress(data))
            actual = intersect(spans(path, sha, before, after))
            self.assertEqual(actual["overlap_ns"]["candidate_verify"], 2800)
            self.assertEqual(actual["overlap_ns"]["other"], 400)
            after["queue"]["elapsed_us"] += 1
            with self.assertRaisesRegex(ValueError, "histograms"):
                spans(path, sha, before, after)

    def test_admission_checks_share_one_counter(self):
        before = {"queue": {"buckets": [0], "elapsed_us": 0}, "execution": {},
                  "checks": 1, "verification_us": 2}
        after = {"queue": {"buckets": [1], "elapsed_us": 6}, "execution": {},
                 "checks": 3, "verification_us": 7}
        data = b"".join(RECORD.pack(*row) for row in [
            (b"A", 0, 2000), (b"R", 2000, 5000), (b"A", 6000, 8000), (b"Q", 2000, 8000)])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "node.trace.xz"
            path.write_bytes(lzma.compress(data))
            result = intersect(spans(path, hashlib.sha256(data).hexdigest(), before, after))
        self.assertEqual(result["overlap_ns"]["client_check"], 2000)
        self.assertEqual(result["overlap_ns"]["relay_check"], 3000)
        self.assertEqual(result["overlap_ns"]["other"], 1000)

    def test_candidate_subdivision_matches_parent_and_excludes_prefunding(self):
        before = {"queue": {"buckets": [0], "elapsed_us": 0}, "checks": 0, "verification_us": 0,
                  "execution": {"candidate_verify": {"buckets": [1], "elapsed_us": 5}}}
        after = {"queue": {"buckets": [1], "elapsed_us": 3}, "checks": 0, "verification_us": 0,
                 "execution": {"candidate_verify": {"buckets": [2], "elapsed_us": 10}}}
        rows = []
        for origin in (0, 6000):
            for i, tag in enumerate((b"d", b"b", b"e", b"f", b"w")):
                rows.append((tag, origin + 1000*i, origin + 1000*(i+1)))
            rows.append((b"V", origin, origin + 5000))
        rows.append((b"Q", 8000, 11000))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "node.trace.xz"
            for expected_error in (False, True):
                if expected_error:
                    rows[8] = (b"e", 8100, 9000)
                data = b"".join(RECORD.pack(*row) for row in rows)
                path.write_bytes(lzma.compress(data))
                parts = {}
                sha = hashlib.sha256(data).hexdigest()
                if expected_error:
                    with self.assertRaisesRegex(ValueError, "partition"):
                        spans(path, sha, before, after, parts)
                else:
                    phases = spans(path, sha, before, after, parts)
                    self.assertEqual(parts["finish_block"], [(9000, 10000)])
                    result = intersect({**parts, "queue": phases["queue"]})
                    self.assertEqual(result["overlap_ns"]["finish_block"], 1000)
                    self.assertEqual(result["overlap_ns"]["other"], 0)


if __name__ == "__main__":
    unittest.main()

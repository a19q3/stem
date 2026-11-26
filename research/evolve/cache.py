"""Finite success-cache policies with an independent replay oracle for charged cycles."""

from collections import Counter, OrderedDict, deque
import hashlib
import json
import lzma
from pathlib import Path
import random
import sys


class Lru:
    def __init__(self, slots):
        self.slots = slots
        self.entries = OrderedDict()

    def get(self, key):
        if key in self.entries:
            self.entries.move_to_end(key)
        return self.entries.get(key)

    def put(self, key, entry):
        self.entries[key] = entry
        self.entries.move_to_end(key)
        if len(self.entries) > self.slots:
            self.entries.popitem(last=False)


class Sharded:
    def __init__(self, slots):
        self.shards = [Lru(slots // 4) for _ in range(4)]

    def shard(self, key):
        return self.shards[int(key[:8], 16) % 4]

    def get(self, key):
        return self.shard(key).get(key)

    def put(self, key, entry):
        self.shard(key).put(key, entry)


class TwoLevel:
    """Four modeled workers; total resident slots include duplicated entries."""

    def __init__(self, slots):
        per_worker = max(1, slots // 16)
        self.local = [Lru(per_worker) for _ in range(4)]
        self.shared = Lru(slots - 4 * per_worker)
        self.worker = 0

    def get(self, key):
        worker = self.local[self.worker]
        item = worker.get(key)
        if item is None:
            item = self.shared.get(key)
            if item is not None:
                worker.put(key, item)
        return item

    def put(self, key, entry):
        self.local[self.worker].put(key, entry)
        self.shared.put(key, entry)


class Sieve:
    def __init__(self, slots):
        self.slots = slots
        self.entries = {}
        self.order = deque()
        self.visited = set()

    def get(self, key):
        if key in self.entries:
            self.visited.add(key)
        return self.entries.get(key)

    def put(self, key, entry):
        if key not in self.entries:
            if len(self.entries) == self.slots:
                while self.order[0] in self.visited:
                    candidate = self.order.popleft()
                    self.visited.remove(candidate)
                    self.order.append(candidate)
                del self.entries[self.order.popleft()]
            self.order.append(key)
        self.entries[key] = entry


class S3Fifo:
    def __init__(self, slots):
        self.slots = slots
        self.small = OrderedDict()
        self.main = OrderedDict()
        self.ghost = OrderedDict()

    def get(self, key):
        item = self.small.get(key) or self.main.get(key)
        if item is not None:
            item[1] = min(3, item[1] + 1)
            return item[0]

    def put(self, key, entry):
        item = self.small.get(key) or self.main.get(key)
        if item is not None:
            item[0] = entry
            return
        if len(self.small) + len(self.main) == self.slots:
            while len(self.small) > max(1, self.slots // 10):
                k, v = self.small.popitem(last=False)
                if v[1] >= 2:
                    self.main[k] = [v[0], 0]
                else:
                    self.ghost[k] = True
                    if len(self.ghost) > self.slots:
                        self.ghost.popitem(last=False)
                    break
            while len(self.small) + len(self.main) == self.slots:
                k, v = self.main.popitem(last=False)
                if v[1] == 0:
                    break
                self.main[k] = [v[0], v[1] - 1]
        if self.ghost.pop(key, None):
            self.main[key] = [entry, 0]
        else:
            self.small[key] = [entry, 0]


def replay(trace, policy, slots, ignore_parent=False):
    cache = policy(slots)
    hits = 0
    avoided_cycles = 0
    check = hashlib.sha256()
    for pos, (key, parent, cycles, sensitive, limit, _) in enumerate(trace):
        if isinstance(cache, TwoLevel):
            cache.worker = pos % 4
        cached = cache.get(key)
        if cached and (ignore_parent or cached[0] is None or cached[0] == parent):
            cost = cached[1]
            hits += 1
            avoided_cycles += cost
        else:
            cost = cycles
            if cycles <= limit:
                cache.put(key, (parent if sensitive else None, cycles))
        observed = cost if cost <= limit else -1
        expected = cycles if cycles <= limit else -1
        if observed != expected:
            raise ValueError(f"cycle mismatch at event {pos}")
        check.update(observed.to_bytes(8, "little", signed=True))
    return dict(
        events=len(trace),
        hits=hits,
        nominal_cycles_on_hits=avoided_cycles,
        decisions_sha256=check.hexdigest(),
    )


def synthetic(seed):
    rng = random.Random(seed)
    trace = []
    for i in range(8192):
        slot = rng.randrange(16 if (i // 256) % 3 else 512)
        key = hashlib.sha256(slot.to_bytes(2, "little")).hexdigest()
        parent = (i // 128) % 5
        sensitive = slot % 5 == 0
        cycles = 100 + slot + (parent * 17 if sensitive else 0)
        trace.append(
            (
                key,
                parent,
                cycles,
                sensitive,
                cycles - 1 if i % 53 == 0 else 1000,
                "modeled",
            )
        )
    return trace


def run():
    with lzma.open(Path(__file__).with_name("corpus.json.xz"), "rt") as f:
        corpus = json.load(f)
    traces = {
        name: v["verification"]
        for name, v in corpus["replicas"].items()
        if v["verification"]
    }
    traces.update({f"synthetic-{seed}": synthetic(seed) for seed in (3181, 4219)})
    mutant = [("a" * 64, 0, 10, True, 50, None), ("a" * 64, 1, 30, True, 20, None)]
    try:
        replay(mutant, Lru, 4, ignore_parent=True)
    except ValueError:
        pass
    else:
        raise AssertionError("invalid cache policy accepted")
    result = {}
    for name, trace in traces.items():
        rows = []
        for slots in (16, 64, 256):
            for policy in [Lru, Sharded, Sieve, S3Fifo, TwoLevel]:
                rows.append(
                    dict(
                        slots=slots,
                        policy=policy.__name__,
                        **replay(trace, policy, slots),
                    )
                )
        assert len({r["decisions_sha256"] for r in rows}) == 1
        result[name] = rows
    lengths = Counter(
        length
        for replica in corpus["replicas"].values()
        for _, length in replica["sort_lengths"]
    )
    return dict(
        traces=result, sort_lengths=lengths, rejected_mutant_observations=mutant
    )


if __name__ == "__main__":
    json.dump(run(), sys.stdout, sort_keys=True)

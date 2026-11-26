"""Enumerate erasure patterns and time matched encode and authenticated reconstruction."""

import hashlib
import itertools
import json
import math
import os
import platform
import time
import tracemalloc

import numpy as np
from codec import (
    CHECKS,
    RS,
    apply,
    inverse,
    parity_encode,
    parity_repair,
    parity_repair_in_place_xor,
    peel,
    rs_encode,
    rs_repair,
)


def erasures():
    counts = []
    for count in range(17):
        patterns = list(itertools.combinations(range(16), count))
        counts.append(
            dict(
                erased=count,
                patterns=len(patterns),
                parity_recoverable=sum(
                    peel(pattern) is not None for pattern in patterns
                ),
                rs_recoverable=len(patterns) if count <= 7 else 0,
            )
        )
    return counts


def timed(function, *args):
    start = time.perf_counter_ns()
    out = function(*args)
    return out, time.perf_counter_ns() - start


def trials(width):
    data = np.random.default_rng(341).integers(0, 256, (9, width), dtype=np.uint8)
    digest = hashlib.sha256(data.tobytes()).hexdigest()
    codecs = [
        ("rs", rs_encode, rs_repair, list(range(9))),
        ("parity", parity_encode, parity_repair, [0, 1, 2, 4, 5, 6, 8, 9, 10]),
        (
            "parity_direct",
            parity_encode,
            parity_repair_in_place_xor,
            [0, 1, 2, 4, 5, 6, 8, 9, 10],
        ),
    ]
    rows = []
    for trial in range(8):
        for name, encode, repair, systematic in (
            codecs if trial % 2 == 0 else reversed(codecs)
        ):
            encoded, encode_ns = timed(encode, data)
            for lost in [(0,), (1, 5, 7), (0, 1, 4, 5)]:
                damaged = encoded.copy()
                damaged[list(lost)] = 0
                fixed, ns = timed(repair, damaged, lost)
                if fixed is not None:
                    assert np.array_equal(fixed, encoded)
                    assert (
                        hashlib.sha256(fixed[systematic].tobytes()).hexdigest()
                        == digest
                    )
                else:
                    assert name.startswith("parity") and lost == (0, 1, 4, 5)
                rows.append(
                    dict(
                        trial=trial,
                        codec=name,
                        width=width,
                        missing=lost,
                        success=fixed is not None,
                        repair_ns=ns,
                        encode_ns=encode_ns,
                    )
                )
    memory = {}
    for name, encode, repair, _ in codecs:
        word = encode(data)
        word[0] = 0
        tracemalloc.start()
        repair(word, (0,))
        memory[name] = tracemalloc.get_traced_memory()[1]
        tracemalloc.stop()
    return dict(payload_sha256=digest, rows=rows, peak_traced_one_repair_bytes=memory)


def probes():
    # Independently check all 11,440 distinct 9-row decoding submatrices.
    eye = np.eye(9, dtype=np.uint8)
    for rows in itertools.combinations(range(16), 9):
        selected = RS[list(rows)]
        assert np.array_equal(apply(selected, inverse(selected)), eye)
    data = np.arange(9 * 16, dtype=np.uint8).reshape(9, 16)
    original = parity_encode(data)
    altered = original.copy()
    altered[0, 0] ^= 1
    passed = [
        not np.any(np.bitwise_xor.reduce(altered[list(check)], axis=0))
        for check in CHECKS
    ]
    assert passed.count(False) == 2
    other = data.copy()
    other[0, 0] ^= 1
    replaced = parity_encode(other)
    assert all(
        not np.any(np.bitwise_xor.reduce(replaced[list(c)], axis=0)) for c in CHECKS
    )
    assert (
        hashlib.sha256(other.tobytes()).digest()
        != hashlib.sha256(data.tobytes()).digest()
    )
    return dict(
        invertible_rs_subsets=math.comb(16, 9),
        single_symbol_passing_checks=sum(passed),
        available_checks=8,
        three_distinct_checks_miss_probability=math.comb(6, 3) / math.comb(8, 3),
        three_distinct_samples_miss_four_withheld=math.comb(12, 3) / math.comb(16, 3),
        alternate_codeword_passes_all_checks=True,
    )


if __name__ == "__main__":
    print(
        json.dumps(
            dict(
                python=platform.python_version(),
                numpy=np.__version__,
                affinity=sorted(os.sched_getaffinity(0)),
                cpu=platform.machine(),
                patterns=erasures(),
                probes=probes(),
                timings=[trials(w) for w in (1024, 8192, 65536)],
            ),
            sort_keys=True,
        )
    )

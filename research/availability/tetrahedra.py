"""Top-dimensional parity on a small four-partite complex. Each symbol is one shard."""

import itertools
import json
import math
import random
from collections import Counter

import numpy as np

from codec import apply, exp, inverse

FACETS = list(itertools.product(range(3), repeat=4))
INFO = list(itertools.product(range(2), repeat=4))
# Value 2 in a coordinate selects the XOR of information symbols with either bit.
ROWS = [
    sum(
        1 << i
        for i, data in enumerate(INFO)
        if all(v == 2 or v == bit for v, bit in zip(face, data))
    )
    for face in FACETS
]
TRIANGLES = [
    tuple(
        i
        for i, face in enumerate(FACETS)
        if all(face[a] == b for a, b in zip(axes, fixed))
    )
    for axes in itertools.combinations(range(4), 3)
    for fixed in itertools.product(range(3), repeat=3)
]
CHECKS = [sum(1 << i for i in ids) for ids in TRIANGLES]
ALL = (1 << 81) - 1


def rank(available):
    basis = {}
    for i, row in enumerate(ROWS):
        if (available >> i) & 1:
            while row:
                pivot = row.bit_length() - 1
                if pivot not in basis:
                    basis[pivot] = row
                    break
                row ^= basis[pivot]
    return len(basis)


def peel(missing):
    while missing:
        before = missing
        for check in CHECKS:
            lost = check & missing
            if lost and not lost & (lost - 1):
                missing ^= lost
        if missing == before:
            break
    return missing


def main():
    assert len(TRIANGLES) == 108 and all(len(c) == 3 for c in TRIANGLES)
    assert all(ROWS[a] ^ ROWS[b] ^ ROWS[c] == 0 for a, b, c in TRIANGLES)
    assert rank(ALL) == 16
    columns = [
        sum(1 << i for i, row in enumerate(ROWS) if row & 1 << bit) for bit in range(16)
    ]
    weights = Counter()
    word = 0
    for i in range(1, 1 << 16):
        word ^= columns[(i & -i).bit_length() - 1]
        weights[word.bit_count()] += 1
    assert min(weights) == 16 and weights[16] == 81
    rs = np.array(
        [[exp[(i * j) % 255] for j in range(16)] for i in range(81)], dtype=np.uint8
    )
    rs = apply(rs, inverse(rs[:16]))
    data = np.random.default_rng(671).integers(0, 256, (16, 4096), dtype=np.uint8)
    coded = apply(rs, data)
    tensor = np.array(
        [
            np.bitwise_xor.reduce(
                [data[i] for i in range(16) if row & (1 << i)], axis=0
            )
            for row in ROWS
        ]
    )
    assert all(
        np.array_equal(tensor[a], tensor[b] ^ tensor[c]) for a, b, c in TRIANGLES
    )
    pairs = list(itertools.combinations(range(3), 2))
    for parts in itertools.product(pairs, repeat=4):
        box = {FACETS.index(f) for f in itertools.product(*parts)}
        mask = sum(1 << i for i in box)
        assert peel(mask) == mask and rank(ALL ^ mask) == 15
        live = [i for i in range(81) if i not in box][:16]
        assert np.array_equal(apply(inverse(rs[live]), coded[live]), data)
    rng = random.Random(671)
    observations = []
    for count in [1, 8, 16, 32, 48, 56, 64, 65, 66]:
        result = Counter()
        for _ in range(1024):
            mask = sum(1 << i for i in rng.sample(range(81), count))
            unique = rank(ALL ^ mask) == 16
            local = peel(mask) == 0
            assert not local or unique
            result["unique"] += unique
            result["local"] += local
        observations.append(
            dict(erased=count, trials=1024, rs_mds=1024 if count <= 65 else 0, **result)
        )
    print(
        json.dumps(
            dict(
                faces=81,
                triangles=108,
                single_corruption_failed_checks=4,
                information=16,
                nonzero_weights=dict(sorted(weights.items())),
                box_failures=81,
                rs_reconstructed_boxes=81,
                shard_bytes=4096,
                seed=671,
                random_masks=observations,
                preprint_vertex_lower_bound_for_d3_m_at_least_2=7 * 5**21,
                three_random_checks_miss_one_corruption=math.comb(104, 3)
                / math.comb(108, 3),
            ),
            indent=2,
        )
    )


if __name__ == "__main__":
    main()

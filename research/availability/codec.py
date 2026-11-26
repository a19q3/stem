"""Two systematic (16, 9) erasure codes. Stripe positions are independent bytes."""

import numpy as np

# GF(256) with primitive polynomial x^8+x^4+x^3+x^2+1.
exp = [1]
for _ in range(254):
    value = exp[-1] << 1
    exp.append(value ^ 0x11D if value & 256 else value)
assert len(set(exp)) == 255
log = {value: index for index, value in enumerate(exp)}
MUL = np.zeros((256, 256), dtype=np.uint8)
for a in range(1, 256):
    for b in range(1, 256):
        MUL[a, b] = exp[(log[a] + log[b]) % 255]


def inverse(matrix):
    n = len(matrix)
    rows = np.concatenate((matrix.copy(), np.eye(n, dtype=np.uint8)), axis=1)
    for col in range(n):
        pivot = next(i for i in range(col, n) if rows[i, col])
        rows[[col, pivot]] = rows[[pivot, col]]
        rows[col] = MUL[exp[(-log[int(rows[col, col])]) % 255], rows[col]]
        for i in range(n):
            if i != col and rows[i, col]:
                rows[i] ^= MUL[rows[i, col], rows[col]]
    return rows[:, n:]


def apply(coefficients, data):
    output = np.zeros((len(coefficients), data.shape[1]), dtype=np.uint8)
    for row, weights in zip(output, coefficients):
        for factor, shard in zip(weights, data):
            if factor == 1:
                row ^= shard
            elif factor:
                row ^= MUL[factor, shard]
    return output


def rs_generator():
    vandermonde = np.asarray(
        [[exp[(i * j) % 255] for j in range(9)] for i in range(16)], dtype=np.uint8
    )
    return apply(vandermonde, inverse(vandermonde[:9]))


RS = rs_generator()
assert np.array_equal(RS[:9], np.eye(9, dtype=np.uint8))
CHECKS = [tuple(range(4 * i, 4 * i + 4)) for i in range(4)]
CHECKS += [tuple(range(i, 16, 4)) for i in range(4)]


def rs_encode(data):
    return np.concatenate((data, apply(RS[9:], data)))


def rs_repair(word, missing):
    available = [i for i in range(16) if i not in missing][:9]
    if len(available) < 9:
        return None
    output = word.copy()
    erased = sorted(missing)
    if erased:
        coeffs = apply(RS[erased], inverse(RS[available]))
        output[erased] = apply(coeffs, word[available])
    return output


def parity_encode(data):
    word = np.empty((4, 4, data.shape[1]), dtype=np.uint8)
    word[:3, :3] = data.reshape(3, 3, -1)
    word[:3, 3] = np.bitwise_xor.reduce(word[:3, :3], axis=1)
    word[3] = np.bitwise_xor.reduce(word[:3], axis=0)
    return word.reshape(16, -1)


def peel(missing):
    remaining = set(missing)
    schedule = []
    while remaining:
        before = len(remaining)
        for check in CHECKS:
            absent = remaining.intersection(check)
            if len(absent) == 1:
                i = absent.pop()
                schedule.append((i, [v for v in check if v != i]))
                remaining.remove(i)
        if len(remaining) == before:
            return None
    return schedule


def parity_repair(word, missing):
    steps = peel(missing)
    if steps is None:
        return None
    output = word.copy()
    for i, rest in steps:
        output[i] = np.bitwise_xor.reduce(output[rest], axis=0)
    return output


def parity_repair_in_place_xor(word, missing):
    steps = peel(missing)
    if steps is None:
        return None
    output = word.copy()
    for i, (a, b, c) in steps:
        np.bitwise_xor(output[a], output[b], out=output[i])
        output[i] ^= output[c]
    return output

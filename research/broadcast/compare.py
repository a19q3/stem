"""Paper 2026/2504 Protocol 1 branch selection and synchronous bit-broadcast replay."""

import json
import math
import random


def schedule(size, faults):
    assert size >= 2 and size.bit_count() == 1 and 0 <= faults < size
    k = size.bit_length() - 1
    attempts = 8 * k + 8
    bound = (k + 1) * attempts * (2 * k + 3)  # Claim 14, starts at scale zero.
    return {
        "n": size,
        "t": faults,
        "E": bound,
        "branch": "DS" if faults < bound else "retrieval",
        "rounds": faults + 1
        if faults < bound
        else 1 + 3 * bound * math.ceil(faults / bound),
    }


def broadcast(size, faults, silent, bit):
    """Ideal sender signatures, one bit, permanent crash-stop subset, no cryptographic timings."""
    live = set(range(size)) - silent
    messages = []
    accepted = [set() for _ in range(size)]
    verified = [set() for _ in range(size)]
    total_bytes = total_checks = peak_inbox = 0
    if 0 in live:
        accepted[0].add(bit)
        messages.append((0, bit, (0,)))
    sent = 0
    for round_number in range(1, faults + 2):
        outgoing = []
        inbox_bytes = [0] * size
        for sender, value, signers in messages:
            # One bit packed into a byte, 2-byte signer IDs, 64-byte signatures.
            assert len(signers) == round_number and signers[0] == 0
            packet_bytes = 1 + 66 * len(signers)
            total_bytes += packet_bytes * (size - 1)
            sent += size - 1
            for receiver in live:
                if receiver == sender:
                    continue
                inbox_bytes[receiver] += packet_bytes
                total_checks += len(signers)
                verified[receiver].update((value, signer) for signer in signers)
                if value in accepted[receiver]:
                    continue
                accepted[receiver].add(value)
                if round_number <= faults and receiver not in signers:
                    outgoing.append((receiver, value, (*signers, receiver)))
        peak_inbox = max(peak_inbox, max(inbox_bytes))
        messages = outgoing
        if not messages:
            # The synchronous decision deadline remains t+1.
            break
    decisions = {next(iter(accepted[v])) if len(accepted[v]) == 1 else 0 for v in live}
    assert len(decisions) == 1 and (0 not in live or decisions == {bit})
    return dict(
        decision=decisions.pop(),
        packets=sent,
        body_bytes=total_bytes,
        signature_checks_without_cache=total_checks,
        distinct_signer_bit_checks=sum(map(len, verified)),
        peak_inbox_body_bytes=peak_inbox,
    )


def stem_repair(size, silent, holders, length):
    # GET broadcasts the requested point. Each live archive with that point
    # responds once to the requester. Count only application bodies.
    return dict(
        n=size,
        unavailable=len(silent),
        holders=len(holders),
        payload_bytes=length,
        request_body_bytes=48 * (size - 1),
        response_body_bytes=len(holders) * length,
        completion_message_delays=2 if holders else None,
    )


def main():
    runs, repairs, sweep = [], [], []
    for size in (16, 64, 128, 256):
        branches = [schedule(size, t)["branch"] for t in range(size)]
        assert set(branches) == {"DS"}
        sweep.append(
            dict(n=size, tested_fault_bounds=len(branches), E=schedule(size, 0)["E"])
        )
        rng = random.Random(341 + size)
        indices = list(range(1, size))
        rng.shuffle(indices)
        for faults in sorted({0, 1, (size - 1) // 3, size - 1}):
            cases = {"healthy": set(), "silent_peers": set(indices[:faults])}
            if faults:
                cases["silent_sender"] = {0, *indices[: faults - 1]}
            for name, silent in cases.items():
                for bit in [0, 1]:
                    line = schedule(size, faults)
                    line.update(
                        scenario=name, bit=bit, **broadcast(size, faults, silent, bit)
                    )
                    line["decision_ms_at_10_and_40_ms_rounds"] = [
                        line["rounds"] * x for x in (10, 40)
                    ]
                    runs.append(line)
            silent = set(indices[:faults])
            eligible = set(indices) - silent
            for count in sorted({0, min(1, len(eligible)), len(eligible)}):
                for length in (1024, 65536):
                    repairs.append(
                        stem_repair(size, silent, set(sorted(eligible)[:count]), length)
                    )
    # Unweighted round selector does not inspect stake. Find the first power
    # of two where at least one allowed t would run the retrieval branch.
    n = 2
    while schedule(n, n - 1)["branch"] == "DS":
        n *= 2
    return dict(
        source_pdf_sha256="e35990b558d8f3a33173f598d01b4d216991ad947d1e2900bd2eb7a943acf151",
        sweep=sweep,
        runs=runs,
        repair=repairs,
        first_power_of_two_retrieval=schedule(n, n - 1),
    )


if __name__ == "__main__":
    print(json.dumps(main(), sort_keys=True, allow_nan=False))

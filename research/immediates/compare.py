"""Prove 32-bit B/J extraction rewrites and benchmark the actual release ELF fields."""

from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from elftools.elf.elffile import ELFFile
import z3

ROOT = Path(__file__).resolve().parents[2]
PIN = "fefb8cc01771d59e228c72755ddd1242291f7f22"
SOURCE = "vendor/ckb-vm/src/instructions/utils.rs"


def guest_words(path):
    words = []
    counts = Counter()
    with path.open("rb") as f:
        elf = ELFFile(f)
        assert elf["e_machine"] == "EM_RISCV" and elf.little_endian
        data = elf.get_section_by_name(".text").data()
    offset = 0
    while offset + 2 <= len(data):
        half = int.from_bytes(data[offset : offset + 2], "little")
        if half & 3 != 3:
            counts["compressed"] += 1
            offset += 2
            continue
        assert half & 31 != 31 and offset + 4 <= len(data)
        w = int.from_bytes(data[offset : offset + 4], "little")
        if w & 127 in (0x63, 0x6F):
            words.append(w)
            counts["branch" if w & 127 == 0x63 else "jal"] += 1
        else:
            counts["other32"] += 1
        offset += 4
    assert offset == len(data) and words
    return words, counts


def proof():
    w = z3.BitVec("w", 32)

    def field(lo, width, to):
        return (z3.LShR(w, lo) & ((1 << width) - 1)) << to

    def signed(lo, width, to):
        return ((w << (32 - lo - width)) >> (32 - width)) << to

    original_b = field(8, 4, 1) | field(25, 6, 5) | field(7, 1, 11) | signed(31, 1, 12)
    original_j = (
        field(21, 10, 1) | field(20, 1, 11) | field(12, 8, 12) | signed(31, 1, 20)
    )
    # Same shift/mask expressions appear in the timed scalar candidates below.
    b = (
        ((w >> 19) & 0xFFFFF000)
        | ((w << 4) & 0x800)
        | (z3.LShR(w, 20) & 0x7E0)
        | (z3.LShR(w, 7) & 0x1E)
    )
    j = (
        ((w >> 11) & 0xFFF00000)
        | (w & 0xFF000)
        | (z3.LShR(w, 9) & 0x800)
        | (z3.LShR(w, 20) & 0x7FE)
    )
    out = {}
    for name, ref, alt in [("branch", original_b, b), ("jal", original_j, j)]:
        solver = z3.Solver()
        solver.add(ref != alt)
        assert solver.check() == z3.unsat
        out[name] = "unsat"
    solver = z3.Solver()
    solver.add(b != (b & ~0x800))
    assert solver.check() == z3.sat
    return dict(disagreement=out, rejected_mutant_word=solver.model()[w].as_long())


def main(path):
    src = subprocess.check_output(
        ["git", "show", PIN + ":" + SOURCE], cwd=ROOT, text=True
    )
    originals = []
    for fn in ["x", "xs", "btype_immediate", "jtype_immediate"]:
        found = re.search(r"pub fn " + fn + r"\([^}]+\}", src)
        assert found
        originals.append("#[inline(always)]\n" + found[0])
    words, counts = guest_words(path)
    with tempfile.TemporaryDirectory() as folder:
        tmp = Path(folder)
        (tmp / "words").write_bytes(b"".join(w.to_bytes(4, "little") for w in words))
        (tmp / "bench.rs").write_text(
            "\n".join(originals) + (Path(__file__).with_name("bench.rs")).read_text()
        )
        subprocess.run(
            [
                "rustc",
                "+1.95.0",
                "-C",
                "opt-level=3",
                "--edition=2024",
                tmp / "bench.rs",
                "-o",
                tmp / "bench",
            ],
            check=True,
        )
        raw = subprocess.check_output(
            [tmp / "bench", tmp / "words"], text=True
        ).splitlines()
    return dict(
        reference=PIN,
        elf_sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
        instruction_mix=counts,
        z3=z3.get_version_string(),
        proof=proof(),
        cpu_affinity=sorted(os.sched_getaffinity(0)),
        trials=[json.loads(row) for row in raw],
    )


if __name__ == "__main__":
    print(json.dumps(main(Path(sys.argv[1])), sort_keys=True))

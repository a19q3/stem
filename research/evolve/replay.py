"""Run the pinned finite evaluators inside a private Linux mount and network namespace."""

import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
MANIFEST = json.loads((HERE / "manifest.json").read_text())
ROOT = HERE.parent.parent


def probe(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


MEASURE = """
import json, resource, subprocess, sys
result = json.loads(subprocess.check_output(sys.argv[1:]))
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
result.update(cpu_seconds=usage.ru_utime + usage.ru_stime, peak_rss_kib=usage.ru_maxrss,
              input_blocks=usage.ru_inblock, output_blocks=usage.ru_oublock)
print(json.dumps(result))
"""


def main():
    for target in MANIFEST["targets"].values():
        source = subprocess.check_output(
            ["git", "show", MANIFEST["reference"] + ":" + target["reference_path"]],
            cwd=ROOT,
            text=True,
        )
        assert target["oracle"] in source
    toolchain = Path(
        subprocess.check_output(
            ["rustc", "+" + MANIFEST["rust"], "--print", "sysroot"], text=True
        ).strip()
    )
    cpu = min(os.sched_getaffinity(0))

    def limits():
        resource.setrlimit(resource.RLIMIT_CPU, (60, 60))
        resource.setrlimit(resource.RLIMIT_AS, (3 << 30, 3 << 30))
        os.sched_setaffinity(0, {cpu})

    with tempfile.TemporaryDirectory() as directory:
        prefix = [
            "bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/bin",
            "--setenv",
            "LANG",
            "C",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib",
            "/lib64",
            "--ro-bind",
            str(toolchain),
            "/rust",
            "--ro-bind",
            str(HERE),
            "/src",
            "--bind",
            directory,
            "/work",
            "--tmpfs",
            "/tmp",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--chdir",
            "/work",
            "--",
        ]

        def execute(args):
            return subprocess.check_output(
                prefix + args, preexec_fn=limits, text=True, timeout=75
            )

        execute(
            [
                "/rust/bin/rustc",
                *MANIFEST["sort_flags"],
                "/src/sort.rs",
                "-o",
                "/work/sort",
            ]
        )
        gate = json.loads(execute(["/work/sort", "gate"]))
        cache = json.loads(execute(["/usr/bin/python3", "-I", "/src/cache.py"]))
        sorts = []
        for seed in (3181, 4219):
            for length in range(2, 9):
                for order in (
                    ["oracle", "network", "insertion"],
                    ["insertion", "network", "oracle"],
                ):
                    rows = []
                    for name in order:
                        data = json.loads(
                            execute(
                                [
                                    "/usr/bin/python3",
                                    "-I",
                                    "-c",
                                    MEASURE,
                                    "/work/sort",
                                    str(length),
                                    str(seed),
                                    name,
                                ]
                            )
                        )
                        rows.append(dict(name=name, **data))
                    assert len({r["checksum"] for r in rows}) == 1
                    sorts.append(dict(seed=seed, length=length, rows=rows))
    return dict(
        reference=MANIFEST["reference"],
        gate=gate,
        cache=cache,
        sort=sorts,
        cpu_affinity=cpu,
        host=platform.machine(),
        kernel=platform.release(),
        rustc=subprocess.check_output(
            [str(toolchain / "bin/rustc"), "--version"], text=True
        ).strip(),
        hashes={
            f: probe(HERE / f)
            for f in ("manifest.json", "sort.rs", "cache.py", "corpus.json.xz")
        },
    )


if __name__ == "__main__":
    print(json.dumps(main(), sort_keys=True))

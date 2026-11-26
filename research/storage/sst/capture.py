"""Keep hard links to SSTs created in a disposable Linux load fixture."""

import argparse
import ctypes
import errno
import os
from pathlib import Path
import select
import struct
import time

EVENT = struct.Struct("iIII")
CREATE_OR_MOVE_IN = 0x100 | 0x80
IGNORED = 0x8000
OVERFLOW = 0x4000


def retain(path, destination):
    target = destination / path.parents[2].name / path.parents[1].name / path.name
    target.parent.mkdir(parents=True, exist_ok=True)
    try:
        os.link(path, target)
    except (FileExistsError, FileNotFoundError):
        pass


def capture(scratch, destination, seconds):
    # Install watches before scanning existing files. Short-lived flush outputs
    # can disappear between periodic scans when compaction follows immediately.
    libc = ctypes.CDLL(None, use_errno=True)
    fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
    if fd < 0:
        raise OSError(ctypes.get_errno(), "inotify_init1")
    watches = {}
    end = time.monotonic() + seconds
    try:
        while time.monotonic() < end:
            for directory in scratch.glob("*/evidence/stem-storage-*/node-*/db"):
                if directory in watches.values():
                    continue
                wd = libc.inotify_add_watch(fd, os.fsencode(directory), CREATE_OR_MOVE_IN)
                if wd < 0:
                    if ctypes.get_errno() == errno.ENOENT:
                        continue
                    raise OSError(ctypes.get_errno(), "inotify_add_watch", directory)
                watches[wd] = directory
                for path in directory.glob("*.sst"):
                    retain(path, destination)
            if not select.select([fd], [], [], 0.1)[0]:
                continue
            events = os.read(fd, 65536)
            offset = 0
            while offset < len(events):
                wd, mask, _, length = EVENT.unpack_from(events, offset)
                offset += EVENT.size
                name = os.fsdecode(events[offset:offset + length].rstrip(b"\0"))
                offset += length
                if mask & OVERFLOW:
                    raise RuntimeError("SST event queue overflowed")
                if mask & IGNORED:
                    watches.pop(wd, None)
                elif wd in watches and name.endswith(".sst"):
                    retain(watches[wd] / name, destination)
    finally:
        os.close(fd)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scratch", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--seconds", type=int, required=True)
    args = parser.parse_args()
    capture(args.scratch, args.destination, args.seconds)

#!/usr/bin/env python3
"""Loopback-only TCP shaping for a reproducible split-host fixture."""

import argparse
import asyncio
import json
import random
from pathlib import Path

CHUNK = 16 * 1024
PENDING = 16
MAX_CONNECTIONS = 96
LOOPBACK = "127.0.0.1"


class Link:
    """One capacity budget shared by both directions of the emulated LAN."""

    def __init__(self, delay, jitter, mbps, seed):
        self.delay = delay / 1000
        self.jitter = jitter / 1000
        self.bytes_per_second = mbps * 1_000_000 / 8
        self.next_free = 0
        self.random = random.Random(seed)

    def delivery_time(self, now, length):
        if self.bytes_per_second:
            self.next_free = max(now, self.next_free) + length / self.bytes_per_second
            now = self.next_free
        return now + self.delay + self.random.uniform(-self.jitter, self.jitter)


async def copy(reader, writer, link):
    loop = asyncio.get_running_loop()
    queue = asyncio.Queue(maxsize=PENDING)

    async def schedule():
        previous = 0
        while data := await reader.read(CHUNK):
            await permit.acquire()
            previous = max(previous, link.delivery_time(loop.time(), len(data)))
            queue.put_nowait((previous, data))
        await queue.put(None)

    async def deliver():
        while (item := await queue.get()) is not None:
            when, data = item
            await asyncio.sleep(max(0, when - loop.time()))
            writer.write(data)
            await writer.drain()
            permit.release()
        writer.write_eof()

    permit = asyncio.Semaphore(PENDING)
    async with asyncio.TaskGroup() as jobs:
        jobs.create_task(schedule())
        jobs.create_task(deliver())


async def serve(routes, link):
    active = 0

    def connect(port):
        async def accept(reader, writer):
            nonlocal active
            if active >= MAX_CONNECTIONS:
                writer.close()
                return
            active += 1
            target = None
            try:
                remote, target = await asyncio.wait_for(
                    asyncio.open_connection(LOOPBACK, port, limit=2 * CHUNK), 2
                )
                async with asyncio.TaskGroup() as jobs:
                    jobs.create_task(copy(reader, target, link))
                    jobs.create_task(copy(remote, writer, link))
            except (OSError, TimeoutError, ExceptionGroup):
                pass
            finally:
                if target is not None:
                    target.close()
                writer.close()
                active -= 1

        return accept

    servers = []
    try:
        for listen, target in routes:
            servers.append(await asyncio.start_server(connect(target), LOOPBACK, listen, limit=2 * CHUNK))
        print("ready", flush=True)
        await asyncio.gather(*(server.serve_forever() for server in servers))
    finally:
        for server in servers:
            server.close()
            await server.wait_closed()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--routes", type=Path, required=True, help="JSON array of [listen_port, node_port]")
    parser.add_argument("--delay-ms", type=float, default=0)
    parser.add_argument("--jitter-ms", type=float, default=0)
    parser.add_argument("--mbps", type=float, default=0, help="aggregate bandwidth, with zero for unbounded")
    parser.add_argument("--seed", type=int, default=1)
    args = parser.parse_args()
    routes = json.loads(args.routes.read_bytes())
    if not (0 <= args.jitter_ms <= args.delay_ms <= 1000 and 0 <= args.mbps <= 10_000):
        parser.error("invalid delay, jitter or bandwidth")
    if not 1 <= len(routes) <= 16 or any(
        len(pair) != 2 or any(type(port) is not int or not 1024 <= port <= 65535 for port in pair)
        for pair in routes
    ):
        parser.error("expected one to sixteen loopback routes with unprivileged ports")
    listeners = {pair[0] for pair in routes}
    if len(listeners) != len(routes) or any(pair[1] in listeners for pair in routes):
        parser.error("listener ports must be distinct from each other and from all destinations")
    asyncio.run(serve(routes, Link(args.delay_ms, args.jitter_ms, args.mbps, args.seed)))


if __name__ == "__main__":
    main()

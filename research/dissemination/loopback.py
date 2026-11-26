"""Send matched 64 KiB payload bursts over established, loopback-only TCP edges."""

import argparse
from collections import deque
import json
import math
import platform
import random
import selectors
import socket
import time

import networkx as nx

from graphs import lifted_graphs
from simulate import profile, routes, stats_for_graph

SIZE = 64 * 1024
# First byte identifies a message. The receiver checks every byte before forwarding.
FRAMES = [bytes([i]) * SIZE for i in range(4)]


class Network:
    def __init__(self, topology, live):
        self.selector = selectors.DefaultSelector()
        self.neighbors = {node: [] for node in sorted(live)}
        self.peers = {}
        with socket.create_server(("127.0.0.1", 0)) as listener:
            for a, b in sorted(topology.edges()):
                if a not in live or b not in live:
                    continue
                outgoing = socket.create_connection(listener.getsockname())
                incoming, _ = listener.accept()
                for sock, u, v in [(outgoing, a, b), (incoming, b, a)]:
                    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                    sock.setblocking(False)
                    self.neighbors[u].append(v)
                    self.peers[u, v] = (sock, deque(), bytearray())
                    self.selector.register(sock, selectors.EVENT_READ, (u, v))

    def close(self):
        self.selector.close()
        for sock, _, _ in self.peers.values():
            sock.close()

    def run(self, root, burst):
        start = time.perf_counter_ns()
        seen = [dict() for _ in range(burst)]
        inflight = sent = origin = duplicate = max_queued = 0

        def receive(u, parent, msg):
            nonlocal inflight, sent, origin, duplicate, max_queued
            if u in seen[msg]:
                duplicate += 1
                return
            seen[msg][u] = time.perf_counter_ns() - start
            for v in self.neighbors[u]:
                if v == parent:
                    continue
                sock, pending, _ = self.peers[u, v]
                if not pending:
                    self.selector.modify(
                        sock, selectors.EVENT_READ | selectors.EVENT_WRITE, (u, v)
                    )
                pending.append(memoryview(FRAMES[msg]))
                assert len(pending) <= burst
                max_queued = max(max_queued, len(pending))
                sent += 1
                origin += u == root
                inflight += 1

        for i in range(burst):
            receive(root, None, i)
        while inflight:
            ready = self.selector.select(5)
            assert ready and time.perf_counter_ns() - start < 180_000_000_000
            for key, events in ready:
                u, v = key.data
                sock, pending, buffer = self.peers[u, v]
                if events & selectors.EVENT_READ:
                    chunk = sock.recv(4 * SIZE)
                    assert chunk
                    buffer.extend(chunk)
                    while len(buffer) >= SIZE:
                        index = buffer[0]
                        assert buffer[:SIZE] == FRAMES[index]
                        del buffer[:SIZE]
                        inflight -= 1
                        receive(u, v, index)
                if events & selectors.EVENT_WRITE and pending:
                    try:
                        count = sock.send(pending[0])
                    except BlockingIOError:
                        continue
                    assert count
                    if count == len(pending[0]):
                        pending.popleft()
                        if not pending:
                            self.selector.modify(sock, selectors.EVENT_READ, key.data)
                    else:
                        pending[0] = pending[0][count:]
        assert all(not queue and not buf for _, queue, buf in self.peers.values())
        received = [len(row) for row in seen]
        assert sent == sum(received) - burst + duplicate
        times = [sorted(t for u, t in row.items() if u != root) for row in seen]
        return dict(
            burst=burst,
            sockets=len(self.peers),
            live=len(self.neighbors),
            received=received,
            sender_payload_bytes=origin * SIZE,
            total_payload_bytes=sent * SIZE,
            duplicates=duplicate,
            peak_queued_frames_per_direction=max_queued,
            p99_ns=[t[math.ceil(0.99 * len(t)) - 1] for t in times],
            last_ns=[t[-1] for t in times],
            drain_ns=time.perf_counter_ns() - start,
        )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sizes", type=int, nargs="+", default=[128, 1024])
    parser.add_argument("--seeds", type=int, nargs="+", default=[409, 877])
    args = parser.parse_args()
    rows = []
    for seed in args.seeds:
        for graph, checks in lifted_graphs(seed, max(args.sizes)):
            n = len(graph)
            if n not in args.sizes:
                continue
            regions, _ = profile(n, n + seed)
            root = random.Random(seed + n).randrange(n)
            _, cuts, boundary = stats_for_graph(graph, checks, regions, seed)
            paths, _ = routes(graph, root, regions, random.Random(seed), cuts, boundary)
            paths["direct"] = nx.Graph((root, v) for v in graph if v != root)
            all_nodes = set(graph)
            others = sorted(all_nodes - {root})
            offline = set(random.Random(seed + 7).sample(others, n // 10))
            for label, absent in [("healthy", set()), ("offline", offline)]:
                live = all_nodes - absent
                order = ["direct", "flood", "delay_pair"]
                for name in order if seed == args.seeds[0] else reversed(order):
                    expected = len(
                        nx.node_connected_component(paths[name].subgraph(live), root)
                    )
                    network = Network(paths[name], live)
                    try:
                        for burst in (1, 4):
                            for trial in range(3):
                                row = network.run(root, burst)
                                assert row["received"] == [expected] * burst
                                rows.append(
                                    dict(
                                        seed=seed,
                                        nodes=n,
                                        route=name,
                                        profile=label,
                                        trial=trial,
                                        **row,
                                    )
                                )
                    finally:
                        network.close()
    print(
        json.dumps(
            dict(
                python=platform.python_version(),
                kernel=platform.release(),
                payload_bytes=SIZE,
                rows=rows,
            )
        )
    )


if __name__ == "__main__":
    main()

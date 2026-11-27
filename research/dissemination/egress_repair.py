"""Controlled auxiliary payload traffic over real loopback TCP."""
from heapq import heappop, heappush
from itertools import count
import argparse
import hashlib
import json
import math
import random
import selectors
from time import perf_counter_ns

import networkx as nx

from graphs import lifted_graphs
from loopback import Network, SIZE
from simulate import delay, profile, routes, stats_for_graph

BURST = 4
REPAIR_NS = 500_000_000
OBSERVATION_NS = 1_500_000_000
REPAIR_BYTES = 96  # modelled request size, not a TCP frame
REPAIR_CAP = 16  # responses per payload


def payload(index):
    content = hashlib.shake_256(f"stem auxiliary fixture {index}".encode())
    return bytes([index]) + content.digest(SIZE - 1)


FRAMES = [payload(i) for i in range(BURST)]
DIGESTS = [hashlib.sha256(frame).digest() for frame in FRAMES]


def exercise(path, initial, root, live, regions, rates, per_link, repair):
    network = Network(path, live)
    try:
        return exchange(network, initial, root, live, regions, rates, per_link, repair)
    finally:
        network.close()


def exchange(network, initial, root, live, regions, rates, per_link, repair):
    start = perf_counter_ns()
    elapsed = lambda: perf_counter_ns() - start
    seen = [{root: 0} for _ in FRAMES]
    pending_events = []
    sequence = count()
    link_free = {}
    responses = [0] * BURST
    total = origin = duplicate = request_count = in_flight = 0
    cross_region = 0

    def event(when, kind, src, dst, message):
        heappush(pending_events, (when, next(sequence), kind, src, dst, message))

    def modeled_delay(src, dst):
        return int(delay(src, dst, regions) * 1_000_000)

    def send(now, src, dst, msg):
        nonlocal origin, total, in_flight, cross_region
        resource = (src, dst) if per_link else (src, None)
        serialization = math.ceil(SIZE * 1_000_000 / rates[src])
        outbound = max(now, link_free.get(resource, 0)) + serialization
        link_free[resource] = outbound
        event(outbound + modeled_delay(src, dst), "release", src, dst, msg)
        origin += SIZE * (src == root)
        total += SIZE
        cross_region += SIZE * (regions[src] != regions[dst])
        in_flight += 1

    def forward(now, src, parent, msg):
        for dst in sorted(initial[src]):
            if dst in live and dst != parent:
                send(now, src, dst, msg)

    for msg in range(BURST):
        forward(0, root, None, msg)
        if repair:
            for node in sorted(live - {root}):
                stagger_ns = ((node * 17 + msg * 23) % 40) * 1_000_000
                event(REPAIR_NS + stagger_ns, "check", node, root, msg)

    while pending_events or in_flight:
        now = elapsed()
        assert now < 15_000_000_000, "auxiliary burst failed to drain"
        while pending_events and pending_events[0][0] <= now:
            _, _, kind, src, dst, msg = heappop(pending_events)
            if kind == "check":
                if src not in seen[msg]:
                    request_count += 1
                    event(now + modeled_delay(src, root), "request", src, root, msg)
            elif kind == "request":
                if responses[msg] < REPAIR_CAP:
                    responses[msg] += 1
                    send(now, root, src, msg)
            else:
                assert kind == "release"
                sock, pending, _ = network.peers[src, dst]
                if not pending:
                    network.selector.modify(
                        sock, selectors.EVENT_READ | selectors.EVENT_WRITE, (src, dst)
                    )
                pending.append(memoryview(FRAMES[msg]))
        timeout = 0.05
        if pending_events:
            timeout = min(timeout, max(0, (pending_events[0][0] - elapsed()) / 1e9))
        for key, events in network.selector.select(timeout):
            u, v = key.data
            sock, pending, buffer = network.peers[u, v]
            if events & selectors.EVENT_READ:
                chunk = sock.recv(4 * SIZE)
                assert chunk
                buffer.extend(chunk)
                while len(buffer) >= SIZE:
                    msg = buffer[0]
                    assert msg < BURST and hashlib.sha256(buffer[:SIZE]).digest() == DIGESTS[msg]
                    del buffer[:SIZE]
                    in_flight -= 1
                    if u in seen[msg]:
                        duplicate += 1
                    else:
                        stamp = elapsed()
                        seen[msg][u] = stamp
                        forward(stamp, u, v, msg)
            if events & selectors.EVENT_WRITE and pending:
                try:
                    written = sock.send(pending[0])
                except BlockingIOError:
                    continue
                assert written
                if written == len(pending[0]):
                    pending.popleft()
                    if not pending:
                        network.selector.modify(sock, selectors.EVENT_READ, (u, v))
                else:
                    pending[0] = pending[0][written:]
    assert in_flight == 0
    assert all(not queue and not buf for _, queue, buf in network.peers.values())
    assert total // SIZE == sum(map(len, seen)) - BURST + duplicate
    latency = sorted(t / 1e6 for entries in seen for node, t in entries.items() if node != root)
    return {
        "live": len(live),
        "delivered": [len(entries) for entries in seen],
        "before_repair": [sum(t < REPAIR_NS for t in entries.values()) for entries in seen],
        "by_deadline": [sum(t <= OBSERVATION_NS for t in entries.values()) for entries in seen],
        "p99_ms": latency[math.ceil(0.99 * len(latency)) - 1],
        "last_ms": latency[-1],
        "origin_bytes": origin,
        "total_bytes": total,
        "cross_region_bytes": cross_region,
        "repair_requests": request_count,
        "modeled_control_bytes": request_count * REPAIR_BYTES,
        "responses": responses,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--nodes", type=int, default=128)
    parser.add_argument("--seeds", type=int, nargs="+", default=[409, 877])
    parser.add_argument("--repeat", type=int, default=2)
    args = parser.parse_args()
    for seed in args.seeds:
        graph, checks = next(
            (graph, checks)
            for graph, checks in lifted_graphs(seed, args.nodes)
            if len(graph) == args.nodes
        )
        regions, rates = profile(args.nodes, seed + args.nodes)
        root = random.Random(seed + args.nodes).randrange(args.nodes)
        _, cuts, boundary = stats_for_graph(graph, checks, regions, seed)
        paths, _ = routes(graph, root, regions, random.Random(seed), cuts, boundary)
        direct = nx.Graph((root, v) for v in graph if v != root)
        paths["direct"] = direct
        all_nodes = set(graph)
        faults = {
            "healthy": set(),
            "random10": set(random.Random(seed + 7).sample(sorted(all_nodes - {root}), args.nodes // 10)),
            "region": {node for node in graph if regions[node] == (regions[root] + 1) % 4},
        }
        for variant, absent in faults.items():
            live = all_nodes - absent
            for per_link in (False, True):
                for repeat in range(args.repeat):
                    names = ("direct", "flood", "delay_pair")
                    for name in names if repeat % 2 == 0 else reversed(names):
                        enclosing = paths[name] if name == "direct" else nx.compose(paths[name], direct)
                        row = exercise(
                            enclosing, paths[name], root, live, regions, rates, per_link,
                            repair=name != "direct",
                        )
                        print(json.dumps(dict(
                            seed=seed, profile=variant, per_link=per_link, repeat=repeat,
                            route=name, nodes=args.nodes, root_rate=rates[root], **row,
                        )), flush=True)


if __name__ == "__main__":
    main()

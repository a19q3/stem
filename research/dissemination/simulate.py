"""One-shot application-payload broadcast on synthetic hosts, without network I/O."""

import argparse
from collections import Counter
from hashlib import sha256
from heapq import heappop, heappush
import json
import math
import platform
import random
from time import perf_counter

import networkx as nx
import numpy as np
import scipy

from graphs import (
    boundaries,
    cut_family,
    lifted_graphs,
    min_degree_tree,
    random_tree,
    spectrum,
)


def profile(size, seed):
    rng = random.Random(seed)
    regions = [i % 4 for i in range(size)]
    rng.shuffle(regions)
    # A shared outbound NIC and symmetric propagation. Milliseconds and bytes/ms.
    return regions, [rng.choice([5000, 12500, 25000]) for _ in regions]


def delay(u, v, regions):
    a, b = sorted((u, v))
    jitter = (a * 47 + b * 127 + a * b) % 17 / 10
    return (
        2 if regions[u] == regions[v] else 22 + 7 * abs(regions[u] - regions[v])
    ) + jitter


def stats_for_graph(graph, checks, regions, seed):
    cuts = cut_family(graph, regions, seed)
    base = boundaries(graph, cuts)
    sizes = np.array([min(len(c), len(graph) - len(c)) for c in cuts])
    data = dict(
        checks,
        n=len(graph),
        edges=graph.number_of_edges(),
        topology_sha256=sha256(
            repr(sorted(tuple(sorted(e)) for e in graph.edges())).encode()
        ).hexdigest(),
        expansion_min_observed=float(np.min(base / sizes)),
        expansion_lower_spectral=(8 - checks["second"]) / 2,
        cuts=len(cuts),
        exhaustive=len(graph) == 16,
    )
    return data, cuts, base


def routes(graph, root, regions, rng, cuts, boundary):
    size = len(graph)
    bfs = nx.bfs_tree(graph, root, sort_neighbors=sorted).to_undirected()
    paths = nx.single_source_dijkstra_path(
        graph, root, weight=lambda u, v, _: delay(u, v, regions)
    )
    latency = nx.Graph((path[-2], path[-1]) for path in paths.values() if len(path) > 1)
    random_primary = random_tree(graph, rng)
    thin = min_degree_tree(graph, rng)
    backup = nx.minimum_spanning_tree(
        nx.Graph(
            (u, v, {"weight": rng.random() + int(thin.has_edge(u, v))})
            for u, v in sorted(graph.edges())
        )
    )
    combined = nx.compose(thin, backup)
    # Use the original latency tree and prefer unused links for a second
    # shortest-path tree. Bound the penalty so a long detour can still lose.
    alternate = nx.single_source_dijkstra_path(
        graph,
        root,
        weight=lambda u, v, _: delay(u, v, regions) + 80 * latency.has_edge(u, v),
    )
    latency_pair = latency.copy()
    latency_pair.add_edges_from(
        (p[-2], p[-1]) for p in alternate.values() if len(p) > 1
    )
    # Union forwarding deduplicates by payload ID on first receipt.
    topologies = {
        "flood": graph,
        "bfs": bfs,
        "shortest_delay": latency,
        "random_tree": random_primary,
        "degree_balanced": thin,
        "two_trees": combined,
        "delay_pair": latency_pair,
    }
    rows = {}
    for name, topology in topologies.items():
        depths = nx.single_source_shortest_path_length(topology, root)
        rows[name] = dict(
            edges=topology.number_of_edges(),
            max_hops=max(depths.values()),
            mean_hops=sum(depths.values()) / (size - 1),
            cut_ratio_max_observed=float(np.max(boundaries(topology, cuts) / boundary)),
        )
    return topologies, rows


def transmit(topology, root, regions, rates, payload, offline, order_seed, pooled):
    """FIFO, store-and-forward, each first receiver forwards once except back to its parent."""
    size = len(regions)
    arrivals = {}
    events = [(0.0, root, -1)]
    free = [0.0] * size
    volume = Counter()
    links = Counter()
    crossing = 0
    while events:
        now, u, predecessor = heappop(events)
        if u in arrivals or u in offline:
            continue
        arrivals[u] = now
        if topology is None:
            peers = list(range(size)) if u == root else []
        else:
            peers = sorted(topology[u])
        random.Random(order_seed + u).shuffle(peers)
        for v in peers:
            if v == u or v == predecessor:
                continue
            start = max(now, free[u]) if pooled else now
            departure = start + payload / rates[u]
            free[u] = max(free[u], departure)
            volume[u] += payload
            links[u, v] += payload
            crossing += int(regions[u] != regions[v]) * payload
            heappush(events, (departure + delay(u, v, regions), v, u))
    times = sorted(t for u, t in arrivals.items() if u != root)
    missing = sorted(set(range(size)) - offline - arrivals.keys())
    # Idealized post-pass repair: one direct response per missing live node. No
    # failure detector or request arbitration is modeled; the delay is optimistic.
    repair = max([free[root], *arrivals.values()]) + 2 * max(
        delay(root, v, regions) for v in range(size)
    )
    repaired = list(times)
    for v in missing:
        departure = repair + payload / rates[root]
        if pooled:
            repair = departure
        repaired.append(departure + delay(root, v, regions))

    def quantile(xs, p):
        return round(sorted(xs)[math.ceil(len(xs) * p) - 1], 4) if xs else None

    weights = [8 if regions[v] == 0 else 1 for v in range(size)]
    return dict(
        live=size - len(offline),
        received=len(arrivals),
        repair_copies=len(missing),
        live_weight=sum(weights[v] for v in range(size) if v not in offline),
        reached_weight=sum(weights[v] for v in arrivals),
        bytes_including_repair=sum(volume.values()) + payload * len(missing),
        sent_bytes=sum(volume.values()),
        source_bytes=volume[root],
        max_egress_bytes=max(volume.values()),
        max_link_bytes=max(links.values()),
        interregion_bytes=crossing,
        p50_reached_ms=quantile(times, 0.5),
        p99_reached_ms=quantile(times, 0.99),
        p99_with_repair_ms=quantile(repaired, 0.99),
    )


def run(seeds):
    records = []
    for seed in seeds:
        for graph, checks in lifted_graphs(seed, 1024):
            size = len(graph)
            if size == 32 or size == 512:
                continue
            regions, rates = profile(size, seed + size)
            root = random.Random(seed + size).randrange(size)
            others = sorted(set(range(size)) - {root})
            offline = set(random.Random(seed + 7).sample(others, size // 10))
            for kind in ["lift", "regular"]:
                if kind == "regular":
                    start = perf_counter()
                    graph = nx.random_regular_graph(8, size, seed=seed)
                    second, least = spectrum(graph)
                    assert nx.is_connected(graph) and nx.number_of_selfloops(graph) == 0
                    assert set(dict(graph.degree()).values()) == {8}
                    checks = dict(
                        second=second,
                        least=least,
                        trials=1,
                        build_seconds=perf_counter() - start,
                    )
                geometry, cuts, boundary = stats_for_graph(graph, checks, regions, seed)
                paths, route_stats = routes(
                    graph, root, regions, random.Random(seed), cuts, boundary
                )
                paths["direct"] = None
                route_stats["direct"] = dict(
                    edges=size * (size - 1) // 2, max_hops=1, mean_hops=1
                )
                results = []
                for name, topology in paths.items():
                    for failures, absent in [
                        ("healthy", set()),
                        ("offline_10pct", offline),
                        (
                            "regional_outage",
                            {
                                v
                                for v in others
                                if regions[v] == (regions[root] + 1) % 4
                            },
                        ),
                    ]:
                        for length in [1024, 65536]:
                            for pooled in [True, False]:
                                metrics = transmit(
                                    topology,
                                    root,
                                    regions,
                                    rates,
                                    length,
                                    absent,
                                    seed,
                                    pooled,
                                )
                                assert (
                                    metrics["received"] + metrics["repair_copies"]
                                    == metrics["live"]
                                )
                                if not absent or topology is None:
                                    assert metrics["repair_copies"] == 0
                                results.append(
                                    dict(
                                        route=name,
                                        failures=failures,
                                        payload=length,
                                        shared_egress=pooled,
                                        **metrics,
                                    )
                                )
                records.append(
                    dict(
                        seed=seed,
                        kind=kind,
                        root=root,
                        geometry=geometry,
                        routes=route_stats,
                        traffic=results,
                    )
                )
    return dict(
        versions=[
            platform.python_version(),
            nx.__version__,
            np.__version__,
            scipy.__version__,
        ],
        records=records,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--seeds", nargs="+", type=int, default=[19, 83, 251])
    args = parser.parse_args()
    print(json.dumps(run(args.seeds), sort_keys=True, allow_nan=False))

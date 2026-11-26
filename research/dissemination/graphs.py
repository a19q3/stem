"""Finite degree-eight graphs, spectral checks and cut measurements for issues 9/10."""

import math
import random
from time import perf_counter

import networkx as nx
import numpy as np
from scipy.sparse.linalg import eigsh


def spectrum(graph):
    matrix = nx.to_scipy_sparse_array(graph, nodelist=range(len(graph)), dtype=float)
    # Fixed start independent of ARPACK's default random state.
    start = np.random.default_rng(2718).normal(size=len(graph))
    if len(graph) <= 32:
        values = np.linalg.eigvalsh(matrix.toarray())
        second, least = values[-2], values[0]
    else:
        top = eigsh(
            matrix, k=2, which="LA", v0=start, return_eigenvectors=False, tol=1e-10
        )
        second = min(top)
        least = eigsh(
            matrix, k=1, which="SA", v0=start, return_eigenvectors=False, tol=1e-10
        )[0]
    return float(second), float(least)


def lifted_graphs(seed, maximum):
    """Search nonbipartite two-lifts of a fixed circulant; verify each finite instance."""
    graph = nx.circulant_graph(16, [1, 2, 3, 4])
    rng = random.Random(seed)
    attempts = 0
    while True:
        started = perf_counter()
        if attempts:
            base = graph
            size = len(base)
            for attempts in range(1, 129):
                graph = nx.Graph()
                graph.add_nodes_from(range(2 * size))
                for u, v in sorted(base.edges()):
                    swap = rng.randrange(2)
                    graph.add_edge(u, v + size * swap)
                    graph.add_edge(u + size, v + size * (1 - swap))
                second, least = spectrum(graph)
                if max(second, -least) < 2 * math.sqrt(7) - 1e-7:
                    break
            else:
                raise RuntimeError(f"No verified lift of order {size} for seed {seed}")
        else:
            second, least = spectrum(graph)
        assert nx.is_connected(graph) and not nx.is_bipartite(graph)
        assert (
            set(dict(graph.degree()).values()) == {8}
            and nx.number_of_selfloops(graph) == 0
        )
        assert max(second, -least) < 2 * math.sqrt(7) - 1e-7
        yield (
            graph,
            dict(
                second=second,
                least=least,
                trials=attempts,
                build_seconds=perf_counter() - started,
            ),
        )
        if len(graph) >= maximum:
            return
        attempts = 1


def boundaries(graph, cuts):
    """Count edges crossing each fixed cut, using indexed endpoint arrays."""
    edges = np.asarray(sorted(graph.edges()), dtype=int)
    result = []
    for members in cuts:
        inside = np.zeros(len(graph), dtype=bool)
        inside[members] = True
        result.append(np.count_nonzero(inside[edges[:, 0]] ^ inside[edges[:, 1]]))
    return np.asarray(result)


def cut_family(graph, regions, seed):
    size = len(graph)
    if size == 16:
        # Each nontrivial cut once, choosing the side that excludes vertex zero.
        masks = np.arange(1, 1 << (size - 1), dtype=np.uint32) << 1
        return [np.flatnonzero(mask & (1 << np.arange(size))) for mask in masks]
    rng = random.Random(seed)
    cuts = [[i] for i in range(size)]
    cuts.extend(
        sorted(nx.single_source_shortest_path_length(graph, i, cutoff=1))
        for i in rng.sample(range(size), min(size, 64))
    )
    cuts.extend([i for i in range(size) if regions[i] == region] for region in range(4))
    cuts.extend(rng.sample(range(size), rng.randint(2, size // 2)) for _ in range(64))
    return cuts


def random_tree(graph, rng):
    edges = list(sorted(graph.edges()))
    rng.shuffle(edges)
    forest = nx.utils.UnionFind()
    tree = nx.Graph()
    tree.add_nodes_from(graph)
    for u, v in edges:
        if forest[u] != forest[v]:
            tree.add_edge(u, v)
            forest.union(u, v)
    return tree


def min_degree_tree(graph, rng):
    """A bounded candidate family. The degree proxy is exact only for singleton cuts."""
    candidates = [random_tree(graph, rng) for _ in range(12)]
    return min(
        candidates,
        key=lambda tree: (
            max(dict(tree.degree()).values()),
            sum(d * d for _, d in tree.degree()),
        ),
    )

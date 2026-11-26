# Batch reads on an immutable authenticated Patricia tree

The example builds the native Cell tree at 1,024, 65,536 and 1,000,000 live keys. For each size it tests hashed keys and keys sharing a 64-bit prefix. The reference uses one ordinary proof traversal per query. The candidate sorts queries, walks the shared prefix once per worker partition, verifies every loaded node hash and restores caller order. Each slice has its own output paths. Proof encoding and the durable editor are the repository implementations.

The 108 cases span 128, 1,024 and 4,096 queries, presence and absence, repeated keys, prefix divergence and 1–32 Rayon workers. All returned values and encoded proof bytes matched the oracle. Additional checks cover the empty root and retained roots across insert, delete and reinsert. A read-only candidate has no write journal or reclamation behavior to validate.

For 1,024 queries on one million hashed keys, the reference read **21,431** immutable node records. The candidate read **11,321** with one partition and **11,353** with four. The shared-prefix workload dropped from **19,904** to **10,671** with four partitions. The example charges array preparation, query sorting, result scattering and worker dispatch to the candidate. The fixture used an in-memory map and four allowed CPUs, so 8–32 workers were oversubscribed. The warmed second traversal and single sample per cell make wall time order-sensitive. The archive includes individual read bytes, times, roots and process resource usage.

**Decision.** Batched immutable reads deserve a RocksDB-backed experiment. Keep subtree mutation and reclamation serial while investigating that read path. These measurements do not establish replicated throughput gains.

```sh
CXXFLAGS='-include cstdint' cargo build --locked --release --example research_patricia
taskset -c 0-3 target/release/examples/research_patricia
```

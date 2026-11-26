# Weighted finality observations and replay

Two native release localnets at fefb8cc ran the real payment lock. The four-member run finalized 24 payments with one replica offline; after restart all four agreed at height 11. The sixteen-member run finalized 48 with two replicas offline; all sixteen agreed at height 19. The fixture verified exact-height headers from each replica and saved two and three consecutive signed proofs respectively. The host ran Linux x86-64 on a Ryzen 9 7950X3D.

The offline checker passes the archived public genesis and each proof through the native finality verifier, independently reconstructs each Header ID and AppHash, checks the parent link between consecutive heights, and compares the last header with the recorded all-replica observation. It reorders and duplicates already verified observations and rejects an inconsistent synthetic output in its own result table. A finite dynamic program finds the minimum overlap among every pair of weighted supermajorities. This is a property of the captured rosters. The observer corpus exercises one restart schedule per roster; it does not establish protocol-wide safety or post-GST liveness.

| Replicas | Total voting power | Minimum intersecting power | Signed headers replayed | Observation orders |
| ---: | ---: | ---: | ---: | ---: |
| 4 | 6 | 4 | 2 | 4 |
| 16 | 24 | 10 | 3 | 18 |

The separate fixed-epoch Rust certificate and durable journal tests both passed. The first release build failed in the pinned upstream RocksDB C++ headers because the system compiler required an explicit cstdint include. The following build and replay use that flag. The archive contains only public fixture inputs, signatures and measurements.

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release --test weighted_finality
python3 research/finality/check.py target/release/stem
```

To regenerate live observations, build the RISC-V lock and use the localnet instructions in OPERATIONS.md. Use 4 validators, 24 transactions, 1 offline and 2 checkpoint proofs, then 16 validators, 48 transactions, 2 offline and 3 checkpoint proofs. Keep each run in a separate output directory. Live scheduling gives different heights and signatures; the archived inputs reproduce the exact recorded proof decisions.

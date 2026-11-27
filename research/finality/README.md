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
git show 12338f2:research/finality/observations.json.xz > /tmp/stem-finality-observations.json.xz
python3 research/finality/check.py target/release/stem /tmp/stem-finality-observations.json.xz
```

To regenerate live observations, build the RISC-V lock and use the localnet instructions in OPERATIONS.md. Use 4 validators, 24 transactions, 1 offline and 2 checkpoint proofs, then 16 validators, 48 transactions, 2 offline and 3 checkpoint proofs. Keep each run in a separate output directory. Live scheduling gives different heights and signatures; the archived inputs reproduce the exact recorded proof decisions.

## Fixed native regression

`cargo test --locked --release --test jolteon_restart_proofs` needs only the public signed fixture in the checkout. The test replays 13 observations through the storage-independent native verifier and checks complete Header bytes, AppHash and recorded parent links. The case set covers the four- and sixteen-member restart runs above, a mixed-version restart and two funded committee replacements. It checks a spent Cell, the original and successor membership paths, execution receipts and proof continuity across both handoffs. An execution proof from the final committee requires the prior handoffs. [Compact fixture](../../tests/fixtures/jolteon_restart_proofs.json) · [Original 4/16-member archive](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/finality/observations.json.xz) · [Mixed restart and handoff archives](../execution-pipeline/README.md#regression-controls)

`tools/jolteon_localnet.py --covered-checkpoint-proofs 3` saves each requested Header, its signed anchor and any intervening parent Headers. Both collection and offline replay authenticate the parent links and compare the requested Header at the exact height. In the mixed sixteen-replica rerun, two replicas restarted and all 48 payments finalized. The certificate returned for height 16 authenticated height 17, and the retained parent authenticated height 16. The standalone checker reproduced all three requested Headers, heights 15 through 17. Consumers that need a certificate for a CKB checkpoint at the exact height use `--checkpoint-proofs`.

These fixtures cover fixed schedules. Systematic event exploration and liveness checks under an explicit synchrony model remain open on issue #8.

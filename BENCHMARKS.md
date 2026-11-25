# Stem measurements

The archived [Fern benchmark record](FERN-BENCHMARKS.md) and its raw artefacts describe commit `29872505350e931a252ddee9f88d94edf410c83a` or the earlier revisions identified in that record. Rates, latency and cycle measurements from those runs are baseline observations.

Stem measurements record the executable, workload, configured limits, offered and acknowledged submissions, successful finalizations and replica visibility.

## Encoding comparison

Each column encodes the same inputs, outputs, scope and 65-byte seal. The two
Molecule layouts and the indexed Telegraph layout run only as test fixtures.
The native network uses inline Telegraph. CKB L1 keeps its original Molecule
schemas and Script hash semantics.

| Workload | Molecule inline | Molecule indexed | Telegraph inline | Telegraph indexed |
| --- | ---: | ---: | ---: | ---: |
| Payment with distinct locks | 467 B | 487 B | 312 B | 318 B |
| Payment with one repeated lock | 467 B | 410 B | 312 B | 261 B |
| One asset output | 459 B | 475 B | 331 B | 337 B |
| Ten asset outputs, distinct locks | 2,421 B | 1,708 B | 1,762 B | 1,165 B |
| Ten asset outputs, repeated lock | 2,421 B | 1,015 B | 1,762 B | 652 B |

The reference CKB L1 payment occupies 460 B. The three-Type-Script fixture
occupies 285 B for the Stem output vector, compared with 590 B for three
separate CKB outputs and their Data vector. A Script Table enables further
compression in batches that share complete Scripts. It also adds reference
checks, table ordering rules and a second step to recover each output's
logical contents. Inline Telegraph avoids these costs and simplifies
standalone Cells and commitment construction.

| Structural validation and hash | Molecule inline | Molecule indexed | Telegraph inline | Telegraph indexed |
| --- | ---: | ---: | ---: | ---: |
| Distinct-lock payment parse | 489.7 ns | 393.2 ns | 36.2 ns | 49.1 ns |
| Distinct-lock payment hash | 577.5 ns | 576.6 ns | 439.4 ns | 439.5 ns |
| Ten asset outputs with distinct locks parse | 1,899.3 ns | 1,180.3 ns | 196.2 ns | 295.3 ns |
| Ten asset outputs with distinct locks hash | 2,608.0 ns | 1,934.3 ns | 1,964.0 ns | 1,402.4 ns |

The median uses five release samples of 2,000 operations per layout.
Structural validation alone does not resolve referenced Cells or execute
scripts. The full indexed Telegraph table check took 96.7 ns for the payment
and 573.5 ns for ten asset outputs. It allocated once, requesting 2 B and
11 B respectively. The borrowed Telegraph structural reads allocated zero
times in both cases. The inline Molecule structural reads requested 712 B
in 18 allocations for the payment and 3,368 B in 90 allocations for the
asset fixture.

```sh
STEM_ENCODING_TIMINGS=1 cargo test --locked --release -p stem-ledger \
  measure_transaction_script_dictionary -- --nocapture
cargo test --locked -p stem-ledger c_and_rust_readers_accept_the_same_canonical_frames
```

## Four-replica payment control

Four local CometBFT 0.38.26 validators and two VM workers per replica.
Each run offered 1,200 independently funded test payments at 120/s.
Both runs used the same 300,000,000 block cycles limit and 100,000,000
transaction cycles limit. The reference commit `f3d3c53` includes the
Stem rename and preserves the original Molecule ledger and k256 lock.
The current run includes Telegraph, host secp256k1, complete signing and
the Live Cell cache. The comparison captures their combined effect.

| Window metric | Renamed reference | Current Stem |
| --- | ---: | ---: |
| Offered / admitted | 1,200 / 1,200 | 1,200 / 1,200 |
| Confirmed on all replicas | 1,009 | 1,200 |
| Offered / admitted TPS | 120.00 / 120.00 | 120.00 / 120.00 |
| Successful confirmation TPS | 32.18 | 103.67 |
| Replica-visible p50 / p95 / p99 | 13.105 / 22.632 / 23.099 s | 1.351 / 1.965 / 2.062 s |
| Client drops | 0 | 0 |
| Aggregate process CPU | 109.03 s | 5.85 s |
| Peak sampled process RSS | 397,979,648 B | 377,999,360 B |
| Process write bytes | 100,859,904 B | 52,473,856 B |
| P2P bytes sent / received | 7,533,848 / 7,534,390 | 5,439,831 / 5,419,590 |

191 reference transactions missed the observation window. Its latency
percentiles describe confirmed transactions. CPU, writes and network
counters include each run's actual drain window. The two rows are research
runs on one host, without a production throughput claim.

```sh
STEM_TEST_COMET=target/stem-tools/cometbft \
STEM_TEST_LOADGEN=target/stem-tools/stem-loadgen \
STEM_TEST_LOAD_RATE=120 STEM_TEST_LOAD_COUNT=1200 STEM_TEST_LOAD_KIND=payment \
cargo test --locked --release --test native_store \
  four_node_native_finality_restart_and_app_hash -- --ignored --nocapture
```

## Fixed update storage comparison

The archive store prepared and synchronously committed 25 blocks per case.
Each measured block consumed eight funded Cells and created eight Cells.
The varying inputs are initial live state size and prior
empty-block height, with the same transaction count, file system and VM
cycle policy. The numbers report median block preparation and separate
durable write latency.

| Initial Live Cells | Prior archive height | Prepare p50 | Sync p50 |
| ---: | ---: | ---: | ---: |
| 514 | 0 | 1,758 µs | 97 µs |
| 8,194 | 0 | 1,951 µs | 229 µs |
| 8,194 | 512 | 1,981 µs | 207 µs |

The two 8,194-Cell runs isolate the tested history increment. The experiment
covers one size range and 512 prior blocks. Larger state and long-lived
RocksDB compaction still require measurement.

```sh
cargo test --locked --release --test native_store \
  measure_commit_with_fixed_changes_and_varying_state_and_history -- --ignored --nocapture
```

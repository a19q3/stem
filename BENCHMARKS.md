# Stem measurements

The archived [Fern benchmark record](FERN-BENCHMARKS.md) and its raw artefacts describe commit `29872505350e931a252ddee9f88d94edf410c83a` or the earlier revisions identified in that record. Rates, latency and cycle measurements from those runs are baseline observations.

Stem measurements record the executable, workload, configured limits, offered and acknowledged submissions, successful finalizations and replica visibility.

## Encoding comparison

Each column encodes the same inputs, outputs, scope and 65-byte seal. The two
Molecule layouts and the indexed Strand layout run only as test fixtures.
The native network uses inline Strand. CKB L1 keeps its original Molecule
schemas and Script hash semantics.

| Workload | Molecule inline | Molecule indexed | Strand inline | Strand indexed |
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
logical contents. Inline Strand avoids these costs and simplifies
standalone Cells and commitment construction.

| Structural validation and hash | Molecule inline | Molecule indexed | Strand inline | Strand indexed |
| --- | ---: | ---: | ---: | ---: |
| Distinct-lock payment parse | 489.7 ns | 393.2 ns | 36.2 ns | 49.1 ns |
| Distinct-lock payment hash | 577.5 ns | 576.6 ns | 439.4 ns | 439.5 ns |
| Ten asset outputs with distinct locks parse | 1,899.3 ns | 1,180.3 ns | 196.2 ns | 295.3 ns |
| Ten asset outputs with distinct locks hash | 2,608.0 ns | 1,934.3 ns | 1,964.0 ns | 1,402.4 ns |

The median uses five release samples of 2,000 operations per layout.
Structural validation alone does not resolve referenced Cells or execute
scripts. The full indexed Strand table check took 96.7 ns for the payment
and 573.5 ns for ten asset outputs. It allocated once, requesting 2 B and
11 B respectively. The borrowed Strand structural reads allocated zero
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
The current run includes Strand, host secp256k1, complete signing and
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


## Additional native workloads

The same four-local-validator harness offered 600 payments at 120/s in each
Type Script workload. The burst offered 1,800 payments at 1,000/s. These
are short windows with drain and unchanged limits. All offered transactions
were admitted and confirmed on all replicas in these runs. Client drops
were zero.

| Workload | Offered / admitted TPS | Confirmed TPS | Visible p50 / p95 / p99 (s) | CPU (s) | Peak RSS (MiB) | Process writes (MiB) | P2P sent / received (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Single | 120 / 120 | 96.05 | 1.211 / 1.811 / 1.921 | 3.22 | 335.2 | 25.4 | 3.53 / 3.53 |
| Double | 120 / 120 | 99.92 | 1.146 / 1.703 / 1.813 | 3.30 | 339.0 | 27.4 | 4.30 / 4.30 |
| Multi | 120 / 120 | 95.07 | 1.386 / 1.985 / 2.097 | 3.70 | 342.3 | 30.1 | 4.90 / 4.85 |
| Settlement | 120 / 120 | 100.93 | 1.168 / 1.753 / 1.835 | 4.20 | 377.1 | 42.9 | 8.21 / 8.20 |
| Payment burst | 1000 / 1000 | 118.66 | 6.391 / 13.370 / 13.442 | 18.54 | 410.9 | 66.6 | 7.88 / 7.87 |

The 1,000/s payment burst accumulated a confirmation queue under the fixed
cycle limit. The individual Type Script rows use different contracts and
are single runs, so their differences do not isolate a marginal Type
Script cost. The full per-run counters, parser measurements and storage
samples are in the [raw measurement archive](benchmarks/2026-11-25-stem-native.json.xz).

## Funded payments during L1 stake handoff

Two local Stem replicas used a real CKB devnet vault deposit to fund the
payments. The run offered and admitted 384 payments at 8.00 TPS, and all
384 became visible on both replicas at 7.57 confirmed TPS. Visibility
p50, p95 and p99 were 1.919 s, 3.445 s and 3.733 s. Client drops were
zero. The sampled window recorded 6.57 CPU seconds, 182,784,000 peak RSS
bytes, 29,601,792 process write bytes and 244,743 / 243,133 P2P bytes
sent / received.

The run applied validator updates at heights 9 and 19, authenticated their
successors and unbonded 250 CKB. The Checkpoint Type ID contract accepted
publications for Stem heights 11 and 24. The [native stake runner](OPERATIONS.md#exercise-the-ckb-stake-registry)
reproduces this workload with its optional funding and load-generator flags.


## Extended payment bursts

Four local replicas used the same cycle limits and 1,800 funded payments per
run. Each request rate was a short delivery burst. Every offered payment
was admitted and confirmed on all replicas with zero client drops.

| Requested delivery TPS | Measured offered / admitted TPS | All-replica confirmed TPS | Visible p50 / p95 / p99 (s) |
| ---: | ---: | ---: | ---: |
| 2,000 | 1,998.88 / 1,998.88 | 118.54 | 6.782 / 13.235 / 14.332 |
| 3,000 | 2,880.44 / 2,880.44 | 118.24 | 6.996 / 13.452 / 14.687 |
| 5,000 | 4,997.24 / 4,997.24 | 116.31 | 7.423 / 13.886 / 15.151 |

Increasing the delivery rate shortened submission and increased time spent
in the queue. Confirmation stayed near 116–119 TPS in these runs. The
[measurement archive](benchmarks/2026-11-25-stem-native.json.xz) includes
process CPU, memory, physical writes and P2P counters for each window.

## Sustained local cycle-budget sweep

Four local CometBFT 0.38.26 validators on an Intel Core i9-13900H, with
two Stem workers per replica and a fresh development genesis for each
run. The runtime for the matched runs was `3aada6d`. Each set offered
equal-shaped, independently funded payments for 30 seconds. Within each
set only `block_cycles` varied. The transaction-count cap was 1,024 and
the per-transaction cycle cap was 100 million throughout.

This harness counts execution at height H after all replicas report
height H+1. The active window is seconds [10, 30) of the scheduled load.
Pending counts include successful admissions that had not yet reached that
confirmation boundary at second 30. The whole-run rate includes drain
and the observation deadline. The active window can count earlier
transactions confirmed after the warm-up, so a short window can exceed
the input rate without demonstrating that rate indefinitely.

| Offered TPS | Block cycles | Active confirmed TPS | p95 cycles per observed block | Pending at 30 s | Whole-run confirmed TPS | Confirmed / sent | Confirmation p99 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 260 | 300 M | 117.0 | 299.6 M | 4,416 | 107.08 | 5,408 / 7,800 | 30.075 s |
| 260 | 600 M | 234.0 | 599.2 M | 1,472 | 218.36 | 7,800 / 7,800 | 6.400 s |
| 260 | 1,200 M | 268.8 | 768.2 M | 632 | 242.45 | 7,800 / 7,800 | 3.308 s |
| 500 | 1,200 M | 436.8 | 1,198.4 M | 3,008 | 416.82 | 15,000 / 15,000 | 6.916 s |
| 500 | 2,400 M | 516.0 | 1,394.3 M | 1,200 | 467.41 | 15,000 / 15,000 | 3.302 s |

All transactions were admitted in these five runs with zero client drops.
Three configurations filled almost all of the cycle budget at p95. The
1,200 M limit had spare capacity at 260 TPS, as did 2,400 M at 500 TPS.
Only confirmed transactions contribute to the latency percentiles.
The 300 M run still had 2,392 transactions awaiting confirmation when its
observation window closed.

The higher budget at 500 TPS reduced aggregate process CPU from 93.48 s
to 45.70 s. Physical process writes were 586.48 MB and 581.75 MB.
CPU includes proposer work, execution, snapshots and all other process
activity over each run's full measurement window. Component-level CPU and
storage saturation require separate profiling.

```sh
STEM_TEST_COMET=target/stem-tools/cometbft \
STEM_TEST_LOADGEN=target/stem-tools/stem-loadgen \
STEM_TEST_LOAD_RATE=260 STEM_TEST_LOAD_COUNT=7800 \
STEM_TEST_BLOCK_TX_LIMIT=1024 STEM_TEST_BLOCK_CYCLES=600000000 \
cargo test --locked --release --test native_store \
  four_node_native_finality_restart_and_app_hash -- --ignored --nocapture
```

A separate 20-second pressure probe offered 1,000 payments/s using the
runtime with the fee-proof query, a 4 billion block cycle budget, and a
2,048-transaction cap. All 20,000 were admitted and confirmed with zero
client drops. The active window measured 1,036.8 confirmed TPS, while the
whole-run rate including drain was **900.52 TPS**. Visibility p50/p95/p99
was 2.982/3.681/3.785 s. The maximum observed block had 1,584 transactions
and 3.042 billion charged cycles, so the higher cycle and transaction
caps still had headroom at this load. The run used 57.01 aggregate CPU
seconds, 1,158,811,648 peak sampled RSS bytes, and 632,471,552 physical
process write bytes. This probe did not establish a CPU or storage
saturation point.

The [cycle sweep archive](benchmarks/2026-11-25-stem-cycle-sweep.json.xz)
contains per-run counters and per-block charged cycles for all six runs.

## Local pressure and replica sweep

One Intel Core i9-13900H host, Rust 1.95.0 and CometBFT 0.38.26. All
runs use signed native payments with one real standard Lock verification,
one distinct funded input each and two VM workers per replica. Each genesis
fixes the byte, count, read and write limits recorded in the archive. The
first series runs the `ee808c9` application binary with the updated load
fixture. All four replicas must expose H+1 before the runner counts a
successful execution at H. The whole-run confirmation rate includes drain.
Rate calculations and per-request timings are in the
[pressure and certificate archive](benchmarks/2026-11-25-stem-evolution.json.xz).

| Block cycles | Commit wait | Target TPS | Attempted TPS | Admitted TPS | Confirmed TPS | Confirmed count | Client drops | p99 visibility |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 B | 1,000 ms | 2,000 | 1,850.32 | 993.71 | 747.49 | 20,734 | 1,240 | 13.255 s |
| 8 B | 1,000 ms | 2,000 | 1,888.77 | 1,134.63 | 861.52 | 22,461 | 2,224 | 10.787 s |
| 8 B | 500 ms | 2,000 | 1,704.40 | 1,193.75 | 926.03 | 23,617 | 5,912 | 9.707 s |
| 8 B | 250 ms | 2,000 | 1,815.20 | 1,165.70 | 943.65 | 23,001 | 3,696 | 9.534 s |
| 8 B | 100 ms | 2,000 | 1,731.51 | 1,255.47 | 1,038.98 | 25,052 | 5,056 | 8.663 s |
| 16 B | 100 ms | 2,000 | 1,871.80 | 1,345.64 | 1,082.39 | 27,314 | 1,832 | 9.889 s |
| 16 B | 100 ms | 5,000 | 2,659.26 | 1,180.45 | 916.14 | 23,340 | 46,592 | 16.275 s |
| 16 B | 100 ms | 10,000 | 3,226.26 | 1,298.08 | 911.99 | 13,204 | 66,800 | 10.820 s |

The 2,000/s runs schedule 40,000 payments over 20 seconds. The higher
targets schedule 100,000 over 20 and 10 seconds. At 4 B the p95 observed
active block charged 3.998 B cycles. At 16 B the 2,000/s probe had p95
5.253 B cycles and 10,729 explicit full-mempool rejections. The 5,000/s
and 10,000/s targets remained dispatch and admission pressure tests.
Observed consensus and application saturation on this shared host depends
on the proposal cadence, queue load and resource limits. These finite
runs do not establish a sustainable network maximum.

A subsequent series held 16 B cycles and 100 ms constant. The new bridge
accounting code was present but the signed payments use development
allocations. Each run used the pre-batching Patricia implementation.
Process writes are Linux `/proc/PID/io` write counters summed by role.
They include all process activity through sampling end. CPU, RSS and
both directions of P2P traffic are archived with the block and admission
records.

| Replicas | Mempool per replica | Target TPS | Attempted TPS | Admitted TPS | Confirmed TPS | Full-mempool replies | Stem writes | CometBFT writes | p99 visibility |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 5,000 | 2,000 | 1,810.91 | 1,338.58 | 1,125.85 | 9,782 | 1,281 MB | 322 MB | 9.186 s |
| 4 | 20,000 | 2,000 | 1,509.06 | 1,506.07 | 353.65 | 62 | 489 MB | 199 MB | 26.802 s |
| 16 | 20,000 | 1,000 | 283.70 | 280.35 | 247.91 | 0 | 461 MB | 489 MB | 15.849 s |
| 32 | 20,000 | 1,000 | 181.76 | 174.15 | 5.30 | 0 | 597 MB | 1,566 MB | 36.991 s |

In the four-replica control, the larger mempool increased RPC admission,
but accumulated more pending work and the CometBFT log recorded proposal
and precommit timeouts. Only 10,784 finished within the observation
window, compared to 27,567 with 5,000 slots. The 16- and 32-replica
probes shared the same host and client and delivered only 28% and 18% of
the requested rate. The 32-replica probe used 10.03 GB of sampled peak
RSS and transferred 2.47 GB in P2P sends. The physical write attribution
currently separates processes. Patricia, history and snapshots still
share application-level storage and counters.

```sh
STEM_TEST_COMET=target/stem-tools/cometbft STEM_TEST_LOADGEN=target/stem-tools/stem-loadgen \
STEM_TEST_EVIDENCE_ROOT=target/stem-runs STEM_TEST_VALIDATORS=4 STEM_TEST_VM_WORKERS=2 \
STEM_TEST_LOAD_COUNT=40000 STEM_TEST_LOAD_RATE=2000 \
STEM_TEST_COMMIT_MS=100 STEM_TEST_MEMPOOL_SIZE=5000 \
STEM_TEST_BLOCK_CYCLES=16000000000 STEM_TEST_BLOCK_TX_LIMIT=8192 \
STEM_TEST_BLOCK_BYTES=8388608 STEM_TEST_CELL_READS=16384 \
STEM_TEST_CELL_READ_BYTES=536870912 STEM_TEST_CELL_WRITE_BYTES=8388608 \
cargo test --locked --release --test native_store \
  four_node_native_finality_restart_and_app_hash -- --ignored --nocapture
```

## Patricia batch update

Paired release runs on the same host, with 25 blocks of eight signed
transactions per configuration. Each transaction consumes and creates
one Cell. The numbers are microseconds at p50. Each configuration uses
a fresh RocksDB. The preparation values include VM execution, tree edits,
and batch preparation. The sync values cover the atomic write.

| Live Cells | Prior blocks | Prepare before | Prepare after | Sync before | Sync after |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 514 | 0 | 1,731 | 1,661 | 98 | 72 |
| 8,194 | 0 | 1,915 | 1,772 | 219 | 146 |
| 8,194 | 512 | 1,963 | 1,717 | 221 | 130 |

A separate four-replica run after the Patricia change reused the 16 B,
100 ms, 5,000-slot and 2,000/s profile above. It attempted 1,902.40 TPS,
admitted 1,774.99 TPS, and confirmed 36,254 payments at 1,522.05 TPS
including drain. Visibility p50/p95/p99 was 2.521/5.641/6.116 s. The
active window confirmed 1,554.08 TPS, with 5,412 pending at the end of the
scheduled window and 1,088 client drops. Compared with the pre-batch
run, Stem process writes were 1,138 MB versus 1,281 MB and CometBFT
writes were 446 MB versus 322 MB. Different blocks and confirmation
counts affect those totals. Both results come from single runs on a
shared machine.

With four VM workers per replica at the same 2,000/s target, the
benchmark attempted 1,908.93 TPS, admitted 1,687.13 TPS, and finalized
34,419 payments at 1,508.57 TPS including drain. Cumulative process CPU
rose from 221.21 s to 270.28 s and p99 rose from 6.116 s to 7.251 s. At
a 5,000/s target and two workers, the client managed 2,168.57 attempted
TPS, 1,307.43 admitted TPS and 1,064.11 confirmed TPS, with 55,792
client drops. These profiles leave two workers as the experiment
default on this host.

```sh
cargo test --locked --release --test native_store \
  measure_commit_with_fixed_changes_and_varying_state_and_history -- --ignored --nocapture
```

## Weighted certificate cost

The fixture repeats committee weights 1, 2, 3, 4 in canonical member
order and signs until the selected weight strictly exceeds two thirds.
It measures Ed25519 verification using dalek and same-message BLS
aggregation in `blst` 0.3.17. Each BLS participant proves possession of
an independently generated key. The RISC-V variant uses
`bls12_381` 0.8.0 on CKB-VM V2 and receives a trusted committee. It
validates the signer bitmap, threshold, curve points and the proof
over the supplied digest. The CKB-VM numbers include parsing and
signature verification, with initialization supplied by the fixture.
CKB transaction overhead and historical committee authentication are
outside this measurement. [CFRG proof-of-possession variant](https://datatracker.ietf.org/doc/draft-irtf-cfrg-bls-signature/)

| Committee | Signers | Indexed Ed25519 | Bitmap Ed25519 | Bitmap BLS | Ed25519 host p50 | BLS host p50 | Ed25519 VM cycles | BLS VM cycles |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 4 | 266 B | 257 B | 97 B | 0.103 ms | 0.783 ms | 8.47 M | 77.02 M |
| 16 | 12 | 794 B | 770 B | 98 B | 0.333 ms | 0.792 ms | 25.96 M | 98.29 M |
| 32 | 23 | 1,520 B | 1,476 B | 100 B | 0.603 ms | 0.759 ms | 49.90 M | 127.53 M |
| 64 | 44 | 2,906 B | 2,824 B | 104 B | 1.185 ms | 0.783 ms | 95.81 M | 183.36 M |
| 100 | 68 | 4,490 B | 4,365 B | 109 B | 1.848 ms | 0.794 ms | 148.08 M | 247.16 M |
| 128 | 87 | 5,744 B | 5,584 B | 112 B | 2.343 ms | 0.797 ms | 189.41 M | 297.66 M |

The bytes are signer payloads. A full QC also needs the signed subject
and view. At 128 members BLS aggregation took 3.219 ms p50 and
registration with validation of all proofs of possession took 107 ms
p50. An individual signature is 64 bytes for Ed25519 and 96 for BLS.
The BLS quorum saves bytes on stored and transmitted certificates.
Equivocation evidence still needs two attributable votes, so the
aggregate cannot replace the individual vote evidence. For these
implementations, Ed25519 used fewer CKB-VM cycles at every size. CometBFT
remains the active protocol. The BLS result is a cost trade-off for a
future direct verifier and weighted network. [Reproduction commands](OPERATIONS.md#exercise-the-native-cometbft-path)

## CKB devnet regression

The CKB and xUDT round trips deployed the L1 vault scripts, paid the
two withdrawal receipts, moved 500,000 shannons between lanes and rotated
the vault. After settlement, the SystemRoot bridge record showed
4,599,200,000,000 shannons in circulating Capacity, zero pending and
2,000,000 unallocated shannons. The runner independently computed the
record amounts from the actual certified deposits and confirmed payouts.
The stake runners separately executed a 250 CKB penalty on CKB, verified
an authenticated committee handoff and a 250 CKB unbonding, and published
CKB checkpoints for Stem heights 11 and 24.

The offline wallet verifier authenticated the original payout output
and the old vault output from the same CKB transaction. The live-cell
RPC reported the old vault as `unknown` after its consumption. The
verifier rejected mismatched Merkle positions and altered witness bytes.

## Paired authorization and Lineage lifecycle

The four-validator CometBFT runner exercised the RISC-V Intent Lock with
two independently signed UDT offers and a separately signed fee sponsor.
The solver attached all three authorizations with the output indices
derived from the assembled transaction. The committed transaction
charged 5,876,118 VM cycles and confirmed on all four validators and
the restored observer. The same runner registered a Lineage and advanced
its head with the original UDT script still present. The separate proof
tool verified each replica's signed successor. The [raw record](benchmarks/2026-11-25-stem-lineage.json)
includes the certificates, lifecycle proof responses and build fingerprint.

The local release workload below uses eight signed head updates per
block over 25 blocks, a real RISC-V marker and UDT script, and fresh
RocksDB copies for serial and four-worker execution. Each value includes
block preparation and synchronous commit. The test compares post-commit
Headers and charged cycles. Signature creation is outside the timer.

| Explicit OutPoint workload | Serial p50 | Four-worker p50 | Mean cycles per block |
| --- | ---: | ---: | ---: |
| Eight independent Lineages | 3.659 ms | 2.355 ms | 15,573,424 |
| Eight pre-built successors of one head | 3.512 ms | 1.717 ms | 15,573,424 |
| One atomic transaction updating eight Lineages | 1.495 ms | 1.066 ms | 2,040,725 |

The one-head workload pre-signs every exact successor. Its results
include repeated working-set reuse and parallel verification of fixed
contexts. It does not measure execution-time lookup, a contended AMM or
concurrent command admission. The atomic transaction shares one Lock
signature across eight inputs under the same Lock, which explains much
of its lower cycle total.

```sh
cargo test --locked --release --test native_store \
  lineage::measure_explicit_lineage_updates -- --exact --ignored --nocapture
```

## Dynamic Lineage commands

Four local CometBFT 0.38.26 validators and a snapshot-restored observer
verified two pre-signed counter commands and a rejected attempt. The
second command resolved a Head advanced by the first command and
updated two Heads atomically. The first and second commands charged
3,870,418 and 3,883,265 cycles. The failed Executor charged 2,021,132
cycles and committed only the fee refund. The Rust runner checked both
Patricia paths per Head and the separate Go verifier checked ten H+1
commits. The [raw record](benchmarks/2026-11-25-stem-dynamic-lineage.json)
contains the outcomes, certificates, public genesis committee, measured
local samples and binary fingerprints.

The local release test used an Intel Core i9-13900H, 31 blocks per
workload, two independent RocksDB instances, identical signed
transactions and four Rayon workers. Preparation includes RISC-V
execution, mandatory Script Groups and building the atomic batch.
Synchronous commit is timed separately. The tests compare Header and
Receipt equality for every block. Measurements are in microseconds.

| Workload | Commands / Head updates per block | Serial prepare p50/p95/p99 | Four-worker prepare p50/p95/p99 | Four-worker commit p50/p95/p99 | Mean charged cycles/block |
| --- | ---: | ---: | ---: | ---: | ---: |
| Independent counters | 8 / 8 | 5470 / 5836 / 6788 | 5009 / 7893 / 8674 | 136 / 213 / 230 | 30,963,344 |
| One contended counter | 8 / 8 | 5156 / 5296 / 5315 | 5197 / 5405 / 7717 | 77 / 93 / 94 | 30,963,344 |
| One atomic eight-target command | 1 / 8 | 1526 / 1677 / 1865 | 1732 / 1948 / 2412 | 51 / 60 / 64 | 3,960,347 |
| Mixed conflicting counters | 8 / 8 | 4853 / 5207 / 5208 | 5005 / 5594 / 10766 | 93 / 132 / 164 | 30,963,344 |
| Constant-product AMM hotspot | 8 / 16 | 7446 / 7927 / 7937 | 7734 / 8315 / 11561 | 82 / 98 / 105 | 31,378,016 |

Independent workers improved median prepare time by about 8% in this
run, while tails increased. Conflicting writes still execute in order.
The AMM uses two separate RISC-V asset groups with fixed development
supplies. Each command updates the pool and the payer's reserve with a
signed minimum amount, so the eight swaps share a common Head. The
correctness test also covers slippage and the rejection of a payer
that lacks the participant reserve's authorization. These are local
block preparation and sync-write timings. Sustained replicated
throughput, concurrent mempool admission and an independent AMM load
generator remain unmeasured.

```sh
cargo test --locked --release --test native_store measure_dynamic_lineage_updates -- --ignored --nocapture
cargo test --locked --release --test native_store measure_lineage_hotspot_amm -- --ignored --nocapture
```

### Executor backend selection

An assembly experiment reused the same per-thread core as the ordinary
Script Group scheduler. Both engines produced identical successors and
cycles, including the same success and exhaustion boundary. The test
alternated backend order over 1,000 samples at each size and timed only
the Executor call.

| Heads | Portable p50/p95/p99 | Reused assembly p50/p95/p99 | Charged cycles |
| --- | ---: | ---: | ---: |
| 1 | 3.946 / 4.367 / 5.344 µs | 25.957 / 30.984 / 33.981 µs | 3,395 |
| 8 | 8.270 / 9.053 / 11.237 µs | 27.234 / 32.121 / 34.060 µs | 14,581 |
| 16 | 13.217 / 14.347 / 16.556 µs | 28.859 / 33.954 / 36.410 µs | 27,365 |

The measured Executor calls favor the portable backend. Whole local
block prepare times varied between runs, and the AMM showed no median
benefit with assembly. Stem continues to use the portable V2 engine for
Executors and the usual backend for validation groups. The [paired raw
record](benchmarks/2026-11-25-stem-executor-backends.json) preserves four
worker and serial measurements. Reproduce the backend experiment in
an isolated checkout of `5b716db`.

```sh
cargo test --locked --release -p stem-vm reusable_executor_matches_portable
cargo test --locked --release -p stem-vm measure_executor_backends -- --ignored --nocapture
```

### Repeated native signature recovery

A single-thread microbenchmark compared 1,000 distinct valid signatures
using the same recovery implementation without caching, with one
cache miss per signature, and with an immediately repeated cache hit.
The cache holds 512 successful recoveries per worker and keys on the
full seal and digest. All three paths returned the same key in every
sample. Wall times below exclude VM setup and instruction dispatch.

| Path | Host recovery p50/p95/p99 |
| --- | ---: |
| No cache | 23.385 / 26.937 / 32.540 µs |
| Cache miss | 21.927 / 23.680 / 26.784 µs |
| Cache hit | 0.033 / 0.044 / 0.050 µs |

The uncached call ran immediately before each measured miss. The
difference between those two rows does not isolate cache overhead.
Every VM syscall still charges 1,900,000 cycles, and the Lock Script
still checks ownership. This benchmark does not estimate cache hit
rates or network throughput.

```sh
cargo test --locked --release -p stem-vm measure_signature_recovery_reuse -- --ignored --nocapture
```

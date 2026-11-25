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

## Native Keeper

Native Keeper was tested against the dynamic Lineage baseline
`f955682`. The Keeper implementation is in `871004b`. Three sequential
four-validator runs used CometBFT 0.38.26 and a fifth replica restored by
remote snapshot. Each submitted two reservation carriers ahead of a
common deadline, then produced a CometBFT block at the deadline with no
submitted transactions. The native block contained two execution
receipts. One command advanced the shared Head and the RISC-V Executor
rejected the other, which committed its authorized fee refund. All
replicas authenticated the same result against a signed successor.
The last run also exported the pending queue proof with its signed
successor.

The local release benchmark used an Intel Core i9-13900H. It alternated
measurement order across two RocksDB directories with identical genesis
parameters. Both had Keeper enabled. At each of 31 heights the active
database executed four pre-signed operations against one Head. The idle
database processed the same heights with an empty queue. Signing and
reservation ingestion occurred before measurement.

| Configured Keeper workload | Prepare p50/p95/p99 | Sync commit p50/p95/p99 | Mean charged cycles per block |
| --- | ---: | ---: | ---: |
| Empty queue | 7 / 9 / 13 µs | 5 / 6 / 8 µs | 0 |
| Four due operations on one Head | 2491 / 2595 / 2673 µs | 76 / 100 / 113 µs | 15,481,672 |

These timings cover local block preparation and synchronous database
commit. The network runs establish correctness, proof agreement and
height-based wake behavior. A sustained multi-replica Keeper load has
yet to be measured. The [raw report and evidence](benchmarks/2026-11-25-stem-keeper.json.xz)
include each network run, public committees and the local comparison.
See the [experiment archive](benchmarks/README.md) for earlier rounds and
rejected experiments.

```sh
cargo test --locked --release --test native_store keeper -- --nocapture
cargo test --locked --release --test native_store measure_keeper_slots -- --ignored --nocapture
STEM_TEST_COMET="$PWD/target/stem-tools/cometbft" STEM_TEST_EVIDENCE_ROOT="$PWD/target/stem-runs" \
  cargo test --locked --release --test native_store four_node_native_finality_restart_and_app_hash \
  -- --ignored --nocapture
STEM_NATIVE_EVIDENCE=/path/to/printed/evidence \
  go -C tools/proof test -mod=readonly -run TestKeeperWakeSignatures -v .
```

## Sponsored bilateral Lineage settlement

This round started from `d459d09`, which includes the one-shot Keeper.
The implementation at `190e7c9` required two independently verified
business seals and a separate fee seal. The signed business terms
bounded the fee and fixed at least one target version. The RISC-V
Executor applied both sides of the exchange, and two native UDT Type
Groups independently checked the two asset totals.

The first network run exposed the fee Lock to the Executor. The adopted
implementation masks that field for sponsored intents, so the Executor
uses the verified business actors and cannot choose output state based
on the fee payer. Both variants were tested on four CometBFT validators
and a snapshot-restored observer. The fixture settled one immediate
exchange and a second one through Keeper. It checked both reserve proofs
on all replicas. The independent Go verifier checked the signed wake
and ten signed reserve-proof responses in each run. Local negative tests
rejected modified amounts and deadlines, reordered actor seals,
a fee above the signed cap, a copied fee seal and a second successful
attempt with a differently owned fee Cell.

The serial local run measured one pre-signed settlement per block for
31 blocks on an Intel Core i9-13900H. Signing was outside the timer.
Both variants encoded the command in 766 bytes.

| Variant | Prepare p50/p95/p99 | Sync commit p50/p95/p99 | Mean charged cycles |
| --- | ---: | ---: | ---: |
| Fee Lock visible to Executor | 1254 / 1342 / 1463 µs | 30 / 39 / 45 µs | 7,735,142 |
| Masked fee field, adopted | 1524 / 2418 / 3116 µs | 36 / 52 / 67 µs | 7,735,690 |

The two local runs show observed timing variation. They do not isolate
the wall-time cost of the field check or measure sustained network
throughput. The [raw report](benchmarks/2026-11-25-stem-sponsored-lineage.json.xz)
preserves both rounds, binary fingerprints, public committees, native
execution and state proofs and independent verifier output.

```sh
cargo test --locked --release --test native_store sponsored_lineage -- --nocapture
cargo test --locked --release --test native_store measure_sponsored_lineage_settlement -- --ignored --nocapture
STEM_TEST_COMET="$PWD/target/stem-tools/cometbft" STEM_TEST_EVIDENCE_ROOT="$PWD/target/stem-runs" \
  cargo test --locked --release --test native_store four_node_native_finality_restart_and_app_hash \
  -- --ignored --nocapture
STEM_NATIVE_EVIDENCE=/path/to/printed/evidence \
  go -C tools/proof test -mod=readonly -run TestSponsoredLineageSignatures -v .
```

## Direct CKB finality

On the local Intel Core i9-13900H, two Stem replicas ran CometBFT 0.38.26
against CKB devnet 0.210.0. The direct contract advanced the unique
checkpoint Cell across 32 signed CometBFT headers. It followed committee
voting powers `[250]`, `[400, 250]`, `[400]`, and `[450]` from
genesis. Relayers used the public commits and native Headers as inputs
to the Go proof exporter. Rust and CKB-VM verified the resulting proof.

| Stem checkpoint height | Consecutive signed commits | Proof bytes | Estimated full CKB transaction cycles | CKB inclusion height |
| --- | ---: | ---: | ---: | ---: |
| 0 | 1 | 707 | 4,234,658 | 51 |
| 8 | 8 | 5,977 | 21,339,974 | 55 |
| 16 | 8 | 6,601 | 36,101,557 | 59 |
| 24 | 8 | 6,291 | 27,745,707 | 63 |
| 31 | 7 | 5,275 | 18,875,683 | 67 |

CKB rejected transactions with a modified signature, insufficient
voting power, a forged committee, a skipped commit, a replayed commit
or an altered native execution root. The first network round used
the latest Header where the genesis Header was required and stopped
before publication. The second reached CKB with a RISC-V atomic
instruction left in the contract and failed VM execution. The successful
round explicitly exported the genesis Header and used the build flags in
the [runbook](OPERATIONS.md#exercise-the-direct-ckb-checkpoint).

A separate probe ran the same Rust verifier in CKB-VM V2 against
Go-signed single-header fixtures. The weighted quorum profile included
the shortest prefix of the power-sorted committee with strictly more
than two-thirds of the power. The cycle counts cover the VM probe;
they exclude a CKB transaction and its other scripts.

| Committee | All signatures | All proof bytes | All cycles | Quorum signatures | Quorum proof bytes | Quorum cycles |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 4 | 889 | 8,941,602 | 2 | 733 | 4,728,375 |
| 16 | 16 | 2,305 | 34,801,904 | 8 | 1,681 | 17,809,734 |
| 32 | 32 | 4,193 | 69,262,400 | 17 | 3,023 | 37,448,333 |
| 64 | 64 | 7,969 | 138,082,882 | 32 | 5,473 | 70,135,930 |
| 100 | 100 | 12,217 | 215,574,594 | 50 | 8,317 | 109,434,187 |
| 128 | 128 | 15,521 | 276,124,141 | 65 | 10,607 | 142,378,670 |

The larger committee figures measure proof verification from an anchored
genesis. The network experiment exercised small committees on shared
hardware. Neither test measures finality latency at scale.

```sh
mkdir -p target/stem-runs/direct-cost
STEM_DIRECT_COST_DIR="$PWD/target/stem-runs/direct-cost" \
  go -C tools/proof test -mod=readonly -run TestDirectCertificateCostFixtures -count=1 .
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --target-dir target/stem-contracts \
  --example direct-finality-cost
STEM_DIRECT_COST_DIR="$PWD/target/stem-runs/direct-cost" \
  STEM_DIRECT_COST_ELF="$PWD/target/stem-contracts/riscv64imac-unknown-none-elf/release/examples/direct-finality-cost" \
  cargo test --locked --release --test direct_finality_cost -- --ignored --nocapture
```

The [raw report](benchmarks/2026-11-25-stem-direct-finality.json.xz)
includes all three network rounds, public headers and votes, rejected
transaction errors, confirmed CKB proofs, 12 VM fixtures and build
fingerprints.

A fourth devnet round forced one validator per RPC page. The runner
fetched 42 pages across CometBFT heights 1–32. Heights 11–20 each
required two pages. All five checkpoints confirmed and all six invalid
submissions were rejected again. Full-transaction estimates ranged from
4,234,032 to 36,237,820 cycles. This exercises actual paginated committee
data from CometBFT. The 128-member cycle figure above still comes from
the VM probe. [Pagination round evidence](benchmarks/2026-11-25-stem-direct-finality-pagination.json.xz)

## Direct bridge withdrawals

Two runs on the Intel Core i9-13900H used a local CKB 0.210.0 devnet
and a single Stem replica with CometBFT 0.38.26. Each round consolidated
real CKB and xUDT on L1, minted on Stem, burned the funded Cells, advanced
a directly verified checkpoint and paid the withdrawals on CKB. Each
release used a separate ordinary CKB fee input. Settlement into Stem
remained guardian-authenticated. Payouts were 119,999,000,000 shannons
on each lane and 1,200 units on the xUDT lane.

| Round | Lane | Burn → Stem checkpoint → settlement | L1 inclusion height | Withdrawal proof bytes | Estimated full CKB transaction cycles |
| --- | --- | --- | ---: | ---: | ---: |
| Initial | CKB | 5 → 6 → 9 | 37 | 486 | 1,855,548 |
| Initial | xUDT | 12 → 13 → 16 | 57 | 486 | 1,912,206 |
| Extended | CKB | 4 → 5 → 8 | 41 | 690 | 1,893,384 |
| Extended | xUDT | 11 → 12 → 15 | 61 | 690 | 1,945,195 |

The cycle figures are CKB RPC estimates for complete release transactions,
including unrelated input verification and xUDT scripts. Checkpoint
publication has its own cost and reuses the published root across
withdrawals. These runs measure individual transactions on local nodes.

The vault rejected an older authenticated root without the pending record,
an omitted checkpoint, a corrupted path, an altered recipient and an
old nonce against a live successor vault. The extended round additionally
rejected a valid direct checkpoint for the same network with a different
Type ID and a one-unit change to the xUDT payout that otherwise balanced.
Both rounds completed a 500,000-shannon reserve move and rotated the
custody multisig. CKB rejected the reserve move when the custody input
was omitted or the paired amounts differed. Reconciliation brought
pending liabilities to zero and preserved the 2,000,000-shannon fee surplus.

The [bridge runbook](OPERATIONS.md#exercise-the-native-bridge) gives the
command, native query and witness builder. The
[raw bridge evidence](benchmarks/2026-11-25-stem-direct-payout.json.xz)
preserves both rounds, signed transactions, CKB inclusion, proof
bundles, node output and executable fingerprints.

### CKB-funded committee regression

The same vault executable supplied confirmed CKB funding for the separate
two-replica stake runner. The first load attempt hit a CometBFT RPC
window at height 17. The block store reported the height before its ABCI
results were readable. The observer was changed to retry that height
without advancing its cursor. The initial attempt has no throughput
measurement.

The rerun offered and admitted 384 payments at 8.00 TPS. All 384 appeared
on both replicas with zero client drops. The rate including drain was
7.57 TPS. The observer recorded p50, p95 and p99 visibility of 2.001,
3.453 and 3.747 seconds. Sampled peak RSS was 178,216,960 bytes, process
writes totaled 31,023,104 bytes and CPU time totaled 6.47 seconds. The
measurement observed both the validator addition and removal. The full
scenario also verified a weight increase to 450, five CKB checkpoints and
250 CKB of delayed unbonding.

This is a low-rate shared-host regression. The
[raw stake evidence](benchmarks/2026-11-25-stem-bridge-funded-stake-rerun.json.xz)
preserves both attempts, the error, the workload, measured events and
the public chain evidence.

## Direct CKB stake retirement

The Stake Registry was exercised on CKB 0.210.0 devnet with two local
Stem replicas and CometBFT 0.38.26. The epoch-2 snapshot wrote a permanent
retirement marker at Stem height 16. The old validator was still active at
height 20 and excluded at height 21. CKB rejected the authenticated
height-16 marker and accepted a fresh proof at height 34. The Registry pins
the exact checkpoint code hash and Type ID.

The final runs paid the retirement transaction from a different CKB Lock
than the stake owner's. Only the owner could withdraw when the relative
20-block delay elapsed. One run returned the full 250 CKB bond. The slash
run froze and forfeited 250 CKB after retirement, then released the
remaining Cell collateral. The double vote used one precommit from a
live CometBFT commit and one offline nil precommit at the same vote
position, both with valid canonical signatures.

| Final round | Operation | Witness payload bytes | Full CKB transaction bytes | Estimated CKB cycles |
| --- | --- | ---: | ---: | ---: |
| Owner withdrawal | Prove retirement | 412 | 1,999 | 2,102,585 |
| Owner withdrawal | Unbond | 65 | 1,166 | 4,192,318 |
| Double vote | Freeze | 349 | 1,899 | 6,034,842 |
| Double vote | Prove retirement | 514 | 2,101 | 2,113,320 |
| Double vote | Finalize slash | 33 | 1,583 | 1,962,072 |
| Double vote | Withdraw storage collateral | 65 | 1,166 | 4,217,194 |

These are complete CKB transaction estimates including fee-input script
verification. The witness payload also includes the operation byte and
validator key. The two retirement proofs have different Patricia path
lengths. CKB rejected the wrong checkpoint Type ID, a missing checkpoint,
a modified path, a guardian certificate, a replay, and early or
unauthorized withdrawal. The guardian journal rejected an observed CKB
anchor after a real devnet reorganization in both final runs. Inclusion
evidence was captured before that test reorganization.

A separate funded run confirmed all 384 payments on both replicas at
7.58 TPS including drain. The offered rate was 8 TPS and p99 visibility
was 3.854 seconds. This low-load regression exercised both committee
updates with real L1 backing. The bridge runner also completed native
CKB and xUDT payouts with the shared checkpoint reader.

The [runbook](OPERATIONS.md#exercise-the-ckb-stake-registry) gives the
reproduction commands. The [raw retirement archive](benchmarks/2026-11-25-stem-direct-retirement.json.xz)
preserves the six successful runs and five fixture failures. It includes
the observed heads and proof payloads, both canonical precommits, signed
CKB transactions, observer evidence, test summaries and executable
fingerprints. Two failures came from long Unix socket paths. Three came
from the CKB-versus-Stem anchor fields in the reorganization fixture.

## Native weighted Jolteon

The fixed-epoch runtime was built from `30e1d71`. The frozen baseline uses
Stem `abddb70` and CometBFT 0.38.26. Both nominal runs used the same
genesis and 32 signed RISC-V payments at each replica count. The offered
rate was 16 transactions/s for two seconds, with two application VM
workers and 300 ms commit or proposal waits. Both set a 2.5 s base
timeout. The operating-system writes came from the same disk-backed
workspace on one 20-logical-core Intel i9-13900H. Each run was scheduled
separately. CometBFT used two Go threads per replica and the baseline
Stem process has its separate admission pool.

| Replicas | Jolteon confirmed/s | CometBFT confirmed/s | Jolteon two-chain proof, bytes |
| ---: | ---: | ---: | ---: |
| 4 | 12.46 | 12.31 | 974 |
| 16 | 12.40 | 12.75 | 2,128 |
| 32 | 12.31 | 12.35 | 3,220 |
| 64 | 12.08 | 5.64 | 6,044 |
| 100 | 11.83 | 1.86 | 9,254 |
| 128 | 12.88 | 0.26 | 11,628 |

Every successful run confirmed 32 payments and checked the execution
Header at a common height. The duration includes draining and observing
at least four new heights. It gives short-workload completion rates under
offered load and does not estimate sustainable throughput or per-payment
latency percentiles. The reported proof consists of the two native Headers
and their bitmap Ed25519 QCs. Membership comes from the trusted genesis.

| Replicas | Jolteon / CometBFT sent MB | Jolteon / CometBFT process write MB | Jolteon / CometBFT CPU seconds | Jolteon / CometBFT sampled RSS GB |
| ---: | ---: | ---: | ---: | ---: |
| 4 | 0.11 / 0.24 | 6.84 / 13.54 | 0.27 / 0.69 | 0.12 / 0.26 |
| 16 | 2.06 / 10.58 | 18.83 / 34.97 | 1.45 / 3.72 | 0.46 / 1.32 |
| 32 | 13.33 / 65.37 | 35.48 / 109.63 | 3.80 / 10.57 | 0.91 / 3.29 |
| 64 | 96.06 / 522.92 | 65.97 / 642.56 | 9.78 / 117.60 | 1.81 / 8.52 |
| 100 | 315.30 / 1,830.79 | 114.36 / 2,045.30 | 19.81 / 436.62 | 2.88 / 16.41 |
| 128 | 649.39 / 6,864.40 | 141.68 / 8,353.10 | 28.39 / 2,203.82 | 3.74 / 23.89 |

Jolteon counts bytes at the application frame layer. CometBFT counts
each persistent P2P connection's monitor bytes. CPU and write counters
are process deltas, and RSS is a summed end sample. Jolteon's process
sample also covers the extra height checks at the end. CometBFT includes
both processes per replica. These numbers also reflect the size of each
replica's local journals and the shared host. Nominal Jolteon framing grew
steeply even at low transaction rates. Full fanout of certificates and
retransmissions remains a scaling cost.

The four-replica fault run killed one unit of voting weight out of six.
The surviving replicas committed 32 payments and all agreed at height
12 after the stopped replica recovered. The 16-replica rerun killed two
Leaders with total weight four out of 24. It saw consecutive TCs at views
17 and 18, committed 64 payments, and all replicas agreed at height 17
after restart. Including the fault wait, the active-replica confirmation
rates were 13.29 and 11.84 payments/s respectively. The offline verifier
accepted the real proof and rejected a modified signature and a proof
checked against a different genesis. Remaining tests include matched
fault runs at every scale, malicious proposal behavior, long-gap sync,
committee change and Jolteon proofs on CKB.

The initial 16-replica fault assertion ran before the two stopped
Leaders had reached their turns. A 32-replica startup failed when replica
6 could not bind its port. The first CometBFT comparator on `/tmp` ran
out of tmpfs quota at 64 replicas and could not start the next two
sizes. The first disk-backed 128-replica run timed out before workload
submission. At cleanup 71 replicas reported height 3 and 57 reported
height 2. The longer retry reached readiness and stopped when the P2P
monitor RPC disconnected. The final retry was interrupted to clean the
workspace target directories. The post-clean rerun used identical
binaries, genesis and signed workload, with 240-second runner wait limits.
All 128 replicas confirmed the 32 payments. The confirmation window was
122.70 seconds. The longer run had sustained host contention and several
CometBFT rounds passed through timeouts. [Completed 128-replica rerun](benchmarks/2026-11-25-stem-comet-128-post-clean.json.xz)

The earlier attempts and their errors remain available. [Public run archive](benchmarks/2026-11-25-stem-jolteon-runtime.json.xz)

A clean rebuild at `ec2b23e` repeated the four-replica test with one
offline validator. All 32 payments finalized, the survivors formed a
TC at view 9, and the restarted replica agreed at height 12. The
workspace run passed 437 tests and left 10 opt-in tests ignored. [Clean rebuild evidence](benchmarks/2026-11-25-stem-jolteon-post-clean.json)

See [commands and timing options](OPERATIONS.md#exercise-the-native-jolteon-path).

## Jolteon verifier in CKB-VM

The portable verifier ran in CKB-VM v0.24.15 assembly mode with VM 2 and
the instruction cost model. Its RISC-V probe read the real two-chain
proofs retained by the nominal network runs and compared the authenticated
parent Header against the offline verification record.

| Members | Proof bytes | Verification cycles |
| ---: | ---: | ---: |
| 4 | 974 | 14,526,510 |
| 16 | 2,128 | 51,764,980 |
| 32 | 3,220 | 87,572,907 |
| 64 | 6,044 | 179,311,080 |
| 100 | 9,254 | 283,988,513 |
| 128 | 11,628 | 360,531,748 |

Changing the final signature in the four-member proof caused rejection.
The measured cycles include the probe's input parsing and construction of
the trusted committee. A complete L1 checkpoint transaction also needs
Type ID, continuity, committee handoff and Cell validation. The reported
proofs contain two QCs, so these figures measure a different workload
from a single signed CometBFT step. [Raw cycle record](benchmarks/2026-11-25-stem-jolteon-ckb-vm.json) [Reproduction commands](OPERATIONS.md#exercise-the-native-jolteon-path)

## CKB-funded Jolteon handoffs

Two disposable CKB devnets accepted Stake Cells before three Stem replicas
started. Each network authenticated its first 250-power voter from CKB.
The join run added the 400-power voter at height 11 and removed the old
voter at height 21. In the replacement run the old roster and new roster
at height 11 were disjoint. The incoming validator then increased its L1
bond from 400 to 450 CKB, and its weight changed at height 21.

An observer stopped at height 6 and missed both transitions. On restart
it requested the old proofs and certified payloads and agreed with the
voters' execution Header at height 25. The outgoing replica also
restarted after leaving the roster. An offline verifier rejected a
modified committee proof and the epoch-2 execution proof supplied
without the second handoff. The independent CKB-VM probe accepted both
handoffs and rejected the omitted handoff in both configurations.

The CKB Type ID contract committed bridge-independent checkpoints at
Stem heights 10, 20 and 25 in each run. The first two publications
verified a handoff and updated the roster commitment. The third verified
a regular two-chain proof under the certified successor. Every update
authenticated the sparse history suffix against the live checkpoint
input. The successor transactions used an independent fee-paying key.
The CKB node rejected a changed output execution root and a handoff
replayed against its already-updated successor.

| Roster scenario | Stem height | CKB witness bytes | CKB node estimated cycles |
| --- | ---: | ---: | ---: |
| Join and exit | 10 | 1,336 | 6,182,155 |
| Join and exit | 20 | 1,286 | 10,313,945 |
| Join and exit | 25 | 885 | 6,035,273 |
| Disjoint replacement and increase | 10 | 1,296 | 6,188,135 |
| Disjoint replacement and increase | 20 | 1,116 | 6,198,077 |
| Disjoint replacement and increase | 25 | 885 | 6,088,341 |

The cost column uses CKB's transaction estimator on the same transaction
subsequently confirmed by CKB. The assembly CKB-VM probe measured
17,084,077 cycles for the join history and 12,989,857 for the replacement
history when it verified two preceding handoffs and the latest execution
proof together. These committee sizes are the functional fixtures.
The 128-member cost remains the separate fixed-epoch proof experiment.

After changing the peer directory, the fixed-epoch regression confirmed
64 real payments on 16 replicas with two consecutive Leaders stopped and
agreed at height 17 after restart. The default CometBFT path separately
passed the CKB retirement and slashing regression and published six
direct checkpoints through Stem height 34. The initial two Jolteon
publication attempts stopped before CKB submission because the runner
had an unbound genesis roster variable. Both are recorded with the
passing reruns. The workspace run passed 438 tests with 10 opt-in tests
ignored. [Public evidence and all nine runs](benchmarks/2026-11-25-stem-jolteon-handoffs.json.xz) [Reproduction commands](OPERATIONS.md#exercise-the-native-jolteon-path)

## Jolteon withdrawals, retirement and slashing

The 2026-11-25 run used four Stem replicas and a local CKB devnet executing
the deployed RISC-V contracts. It exercised confirmed CKB and xUDT deposits,
Jolteon-certified burns, permissionless L1 payouts, and guardian reconciliation
of the observed L1 payouts. The successful reports retain the exact transaction
hashes, proofs and application roots. CKB's `estimate_cycles` was run on the
transactions subsequently accepted by the devnet.

| Bridge route | Payout | Estimated CKB cycles | Patricia witness |
| --- | ---: | ---: | ---: |
| CKB, exact burn Header | 1,199.99 CKB | 1,874,099 | 690 B |
| xUDT, exact burn Header | 1,200 units | 1,922,217 | 588 B |
| CKB, burn ancestry through a later finalized Header | 1,199.99 CKB | 1,866,056 | 690 B |
| xUDT, burn ancestry through a later finalized Header | 1,200 units | 1,944,796 | 588 B |

All four successful bridge runs ended with zero pending Capacity. The runs
also moved reserve between vaults and rotated custody on CKB. The vault
rejected missing or foreign checkpoints, stale roots, invalid Patricia paths,
wrong recipients, nonce replay and incorrect xUDT units. The ancestry run
rejected two tampered MMR proofs at the guardian before it approved
reconciliation. The first bridge attempt sent payouts but exited while
serializing its report. The original diagnostic and signed payout transactions
are in the archive.

| Stake scenario | Freeze cycles | Freeze payload | Retirement cycles | Unbond cycles |
| --- | ---: | ---: | ---: | ---: |
| Voluntary exit after joining | | | 2,085,615 | 4,197,803 |
| Voluntary exit after replacing the genesis committee | | | 2,069,854 | 4,204,840 |
| Slash a genesis voter after committee replacement | 6,351,071 | 1,438 B | 2,085,548 | 4,213,379 |
| Slash a historical epoch 1 voter | 6,652,562 | 1,995 B | 2,084,422 | 4,231,764 |
| Freeze an active epoch 1 voter, then exclude it | 6,627,156 | 1,794 B | 2,095,781 | 4,237,889 |

Each unpenalized run released 250 CKB after retirement and the L1 delay.
Each penalty forfeited all 250 CKB of bonded principal as a CKB transaction
fee. The evidence used one genuine share from a live network QC and a
conflicting vote signed with the controlled devnet validator key. The contract
rejected invalid signatures, valid signatures outside the liability interval,
repeat submissions, early or unfunded penalties, and early or unauthorized
withdrawals. The active-voter test froze a bond before any voluntary exit and
then excluded the voter at Stem height 21 using a new CKB-backed snapshot.
The measured penalty finalization cost was 2,003,457 cycles in that run.

The archived comparison includes the separately built CometBFT contracts
completing the CKB and xUDT round trips and the older direct-checkpoint stake
lifecycle. These runs measure correctness and L1 costs. Throughput was measured
in the separate replica matrix above. The release workspace tests recorded
438 passing, zero failing and 10 ignored.

See the [reproduction commands](OPERATIONS.md#exercise-the-native-jolteon-path)
and [12 archived runs with binary fingerprints and raw evidence](benchmarks/2026-11-25-stem-jolteon-l1-consumers.json.xz).

## Jolteon clock and long-gap recovery

The loopback clock fixtures ran four real replicas. Only the scheduled
leader's realtime clock was shifted by 120 seconds in either direction.
The other replicas produced a timeout certificate for its first view,
rejected the untimely proposal and certified strictly increasing execution
timestamps. Both runs restarted the affected replica with its original
database and finalized the same signed payment at height 6. One early test
assertion expected the first rejection to remain the latest one. The raw
report shows the second failed slot as well.

A matched four-replica run kept one voter offline for more than 220 blocks
while the surviving quorum finalized 32 payments. The previous replay code
exited after passing its 192-candidate branch limit. The new code replayed
225 blocks on the returning replica and reached a common Header at height
230 in 4.06 seconds from restart. The 16-replica regression kept two
consecutive leaders offline, finalized 32 payments and recovered to a common
Header at height 17.

The CKB-backed restart fixture stopped an observer at Stem height 6 and
restarted it after height 244, across two authentic stake-derived handoffs.
That observer replayed 229 blocks, while a retired node replayed 205.
All three agreed at height 249 about 5.08 seconds after restart. A new
observer then rebuilt its RocksDB state from a 15,575-byte snapshot pinned
to a live CKB checkpoint for Stem height 252. It followed the current
committee and matched the active replica's exact Header at height 254.
The importer rejected a different checkpoint pin and an altered state
record. The release workspace suite passed 439 tests, with 10 ignored.

## 128-member checkpoint through CKB P2P

The 128-replica Stem fixture finalized 32 signed payments and all replicas
agreed at Stem height 12. Its real two-chain proof was passed to the full
RISC-V checkpoint contract on two CKB 0.210.0 devnet nodes. Both kept
`max_tx_verify_cycles = 70_000_000` and `min_fee_rate = 1_000`.
The checkpoint transaction was submitted to one node. The second node
received it over CKB P2P, verified it into its pending pool and included
it in a generated block. Both nodes returned the same inclusion proof.

| Metric | Measured result |
| --- | ---: |
| Full CKB checkpoint transaction estimate | 359,104,558 cycles |
| Contract witness payload | 16,994 B |
| Origin submission to the second node's pending pool | 0.702 s |
| Stake members in the signed Stem proof | 128 |

CKB 0.210.0 uses the 70 million threshold to classify transactions for its
verification queues. The verified relay path uses the peer's declared
cycles, while local validation uses the block budget. This test covers one
ordinary checkpoint on an idle, same-host devnet. Full-size committee
handoffs, queue contention and other CKB releases remain unmeasured.
The archive also retains the failed setup attempts while the peer was in
initial block download and when the block-view JSON contained derived fields.
[CKB default policy](https://github.com/nervosnetwork/ckb/blob/119598b0a1146a569068ac80bdba7eb5f6292fd3/resource/ckb.toml#L204-L211)
[CKB verification queue](https://github.com/nervosnetwork/ckb/blob/119598b0a1146a569068ac80bdba7eb5f6292fd3/tx-pool/src/component/verify_queue.rs#L59-L72)
[CKB transaction verification](https://github.com/nervosnetwork/ckb/blob/119598b0a1146a569068ac80bdba7eb5f6292fd3/tx-pool/src/process.rs#L715-L744)
[Reproduction commands](OPERATIONS.md#exercise-the-native-jolteon-path)
[18 successful and failed rounds](benchmarks/2026-11-25-stem-clock-recovery-ckb-relay.json.xz).

## Default Jolteon runtime and ordinary retention

The default `stem run` command was tested with ordinary replicas and one
archive. The most delayed replica in the four-node payment run recovered
226 blocks after more than 220 missed blocks. All four agreed at height
231 within 4.47 seconds from restart. The 64-block ordinary window kept
each local finality index to 64 certificates. A read-only RocksDB scan
found 65–67 retained proposal bodies on ordinary replicas. The archive
held all 272 certificates and 274 proposal bodies at shutdown. The
replicas independently rejected requests for expired data. The archive
served the same old Headers and certificates.

The real CKB stake run exercised two committee handoffs. The late observer
replayed 230 blocks, and the three replicas agreed at height 250 within
5.41 seconds of restart. A fresh ordinary observer restored the
15,607-byte CKB-pinned snapshot at height 253 and followed to height 255.
The restart and restored snapshot ran after the earlier epoch boundary was
outside the ordinary window. A separate CKB run froze, retired and slashed
250 CKB. Four default replicas settled 1,199.99 CKB and 1,200 xUDT units,
reconciled both L1 payouts and returned pending Capacity to zero.

Two independent four-node native composition runs verified the same
dual-Type-Script transfer, rejection of input policy removal, sponsored
two-asset OTX, atomic two-head Lineage, fee-only rollback and a height-32
Keeper wake. The harness authenticated the transaction and receipt Merkle
paths under Jolteon finality and matched all four replicas.

| Four same-host validators, 32 signed payments | Time to all-replica visibility | Payments/s including drain |
| --- | ---: | ---: |
| Default Jolteon runner | 2.572 s | 12.44 |
| CometBFT 0.38.26 comparison build | 2.685 s | 11.92 |

Both runs offered 16 payments/s with 300 ms cadence or commit wait,
2.5-second proposer timeout and the same payments and weights. This
single short pair measures finite workloads. The 16-member Jolteon
fault case finalized all 32 payments with two consecutive leaders
offline and reached a common height 17 after the restart. The first
comparison run reached the Unix socket path limit and succeeded after
moving its work directory to a shorter path. Both logs are archived.

The default release workspace suite passed 438 tests. Twelve opt-in
checks were ignored by that command, including the live composition and
database inspections run separately above. The Go proof tests and the
native systemd unit syntax check also passed. [Reproduction commands](OPERATIONS.md#exercise-the-native-jolteon-path)
[Raw records](benchmarks/2026-11-25-stem-default-jolteon.json.xz).

## CKB fee fund settlement

On an Intel Core i9-13900H host, four default Jolteon replicas and CKB
0.210.0 (119598b) settled a real fee from a bridged CKB Cell. The fixture
paid 800 CKB in Stem fees to put all four accounts above their CKB
occupied Capacity thresholds. It claimed each account through the same
native vault and authenticated its actual CKB payment back into Stem.

| Account | CKB paid | CKB payout estimate | Patricia witness |
| --- | ---: | ---: | ---: |
| Validators and delegators | 520 | 1,870,673 cycles | 588 B |
| DevFund | 120 | 1,887,606 cycles | 690 B |
| Ecosystem Fund | 80 | 1,902,285 cycles | 894 B |
| PoW Security Fund | 80 | 1,868,811 cycles | 690 B |

Each payout transaction includes an independent fee input. The four proofs
rejected a modified Patricia path, omission of the checkpoint, and a
different recipient. Admission rejected an unfunded claim, an overclaim,
a below-capacity claim and a replay. After the last authenticated payout,
the four replicas agreed at height 47. Recognized reserve and circulation
were both 27,468 CKB, with zero pending and unallocated Capacity. The four
payout checkpoints cost 14.31–14.38 million estimated cycles each. This
round measures contract correctness and transaction verification costs.
Its fee and split are fixture settings.

The CKB and xUDT regression rerun passed with ordinary bridge withdrawals,
reserve transfer and rotation. The first regression attempt exposed a
test-runner reference to a variable removed during helper extraction. The
archive preserves that failure with the rerun. The release Rust workspace
suite passed 439 tests, with 12 ignored. The Go proof suite passed.

[Reproduction](OPERATIONS.md#redeem-fee-revenue-on-ckb) ·
[Raw reports, failures and fingerprints](benchmarks/2026-11-25-stem-fee-funds.json.xz).

## Fixed-precision fee calibration

Four local Jolteon replicas finalized 32 signed ordinary payments at 140
shannons each. Each transaction used one input, two outputs, one Cell Dep,
and the standard native secp256k1 Lock. The signed transaction was 312 B.
The 139-shannon variant of the first payment hit its paid cycle limit.
All four replicas agreed at height 15 and the fee tally proof reported
4,480 shannons. The fee schedule used the following integer rates in
millionths of a shannon per unit.

| Deterministic resource | Rate |
| --- | ---: |
| Encoded byte | 200,000 |
| VM cycle | 35 |
| Cell read | 500,000 |
| Cell read byte | 100 |
| Cell write byte | 16,000 |
| Cell deletion | 1,000,000 |

The initial 17,000 rate for a Cell write byte rejected even the 140-shannon
variant. Both rounds are archived. The successful schedule is 30.17% of the
464-shannon CKB reference for this comparison. The reference uses 464 bytes
and the 1,000 shannons/kB default relay rate. The experiment does not sample
CKB mainnet fees or estimate a production security budget.

After updating the genesis commitment for the new price unit, the CKB
devnet regression again settled all four fee payouts and reconciled the
reserve across four replicas at height 49. The Rust workspace release
suite passed 439 tests, with 12 ignored. The opt-in four-replica Polytype,
sponsored OTX, atomic Lineage and Keeper test also passed at height 32.

[Reproduction](OPERATIONS.md#calibrate-native-resource-prices) ·
[Both attempts and the CKB regression](benchmarks/2026-11-25-stem-resource-pricing.json.xz).

## CKB delegation and weighted handoffs

On an Intel Core i9-13900H, three native Stem processes and one CKB
0.210.0 devnet completed two certifying committee handoffs in each run.
The initial committee had one active validator. Its 250 CKB self-bond
and a 100 CKB independently owned delegation produced weight 350 at
genesis. A top-up and a second owner brought delegated principal to
245 CKB. At the next handoff the two raw weights were 495 and 400.
A separate run capped the committee at 800 and verified effective
weights of 442 and 357. The following epoch excluded the departing
validator and restored the remaining validator's raw weight of 400.

| CKB operation | Full transaction bytes | Estimated CKB cycles |
| --- | ---: | ---: |
| Create first delegation | 1,497 | 1,968,353 |
| Top up first delegation | 1,663 | 1,945,639 |
| Create second delegation | 1,611 | 1,957,077 |
| Redeem retired positions | 1,530 each | 2,011,016–2,025,223 |
| Finalize both delegated penalties | 1,461 each | 1,962,379–1,993,295 |

The retirement run returned 175 and 70 CKB of principal plus the
243 CKB occupied Capacity of each position to separate CKB Locks. The
active equivocation run proved and froze the validator on CKB, used a
Stem-certified retirement marker, and transferred the same 245 CKB of
delegated principal to CKB transaction fees. The penalty transactions
each returned 243 CKB to the beneficiary. CKB rejected incorrect or
unfunded delegation weight, a missing owner authorization, an immature
withdrawal, a changed recipient, a frozen withdrawal and replayed vote
evidence. A self-bond-only retirement through a full committee replacement
also passed. This validates settlement for delegation held through a
validator's registration. The per-position occupied Capacity and single
Registry transaction path are measured costs of the current layout.

The release workspace suite passed 439 tests, with 12 ignored. The
Go proof suite passed and the comparison contract compiled for RISC-V.
The archive includes all seven devnet runs and the initial build-test
failure from a missing fixture field.

[Reproduction](OPERATIONS.md#exercise-ckb-delegation) ·
[Raw run and failure evidence](benchmarks/2026-11-25-stem-ckb-delegation.json.xz).

## In-term CKB delegation reductions

The devnet used three Stem processes and CKB 0.210.0 on the i9-13900H
host. Before the first committee handoff, two CKB owners requested
reductions of 100 and 70 CKB. The first position retained 75 CKB of
active principal. With no reductions the old validator's raw voting
power would have been 495. The applied Registry reduced it to 325
at Stem height 11, with the second validator at 400. The old validator
stayed in the committee throughout settlement.

| L1 operation | Transaction bytes | Estimated CKB cycles |
| --- | ---: | ---: |
| Request a reduction | 1,717 | 1,980,074–1,990,936 |
| Confirm the cutoff for each position | 2,229–2,261 | 2,100,741–2,103,460 |
| Withdraw 100 CKB and retain the active position | 1,826 | 1,986,277 |
| Close the 70 CKB position | 1,491 | 1,957,311 |
| Settle the 100 CKB historical double-vote penalty | 3,057 | 6,331,814 |

The normal run paid 100 CKB to the first owner and 337 CKB to the
second, including the second position's 267 CKB occupied Capacity.
The historical run proved a vote fault at height 12 and froze the
validator globally. CKB rejected using that fault against a pending
position with cutoff 11. A separate valid proof of two votes at
height 3 forfeited 100 CKB pending principal, preserving the 75 CKB
active successor. The other owner still collected 337 CKB. The two
runs reached agreement at Stem heights 17 and 18, respectively.

A third workload truncated the actual CKB branch after the two
reduction requests but before the signed snapshot anchor. Both
request Cells remained live. The fixture published an authentic
Stem checkpoint on the replacement CKB chain. CKB rejected the
orphaned Header Dep, and the Stake Registry contract rejected
substituting the new canonical block hash into the old proof.
The normal runs also rejected a checkpoint before the new weights
took effect, missing anchor data, duplicate confirmation, an
immature withdrawal and an attempted recipient change.

The full retirement and active-slash path passed as a regression.
The release Rust workspace passed 440 tests with 12 ignored. The
Go proof suite passed. The archive also keeps the failed historical
fixture attempt and the first successful withdrawal run.

[Run instructions](OPERATIONS.md#exercise-ckb-delegation) ·
[Raw runs and fingerprints](benchmarks/2026-11-25-stem-delegation-reduction.json.xz).

## Compact delegation and position-specific liability

The compact delegation used 154 CKB of occupied Capacity on CKB 0.210.0.
The Cell has 8 Capacity bytes, a 33-byte Lock with empty args, a 65-byte
Type Script with its 32-byte identity commitment, and 48 bytes of
position data. Every transition supplies the identity preimage in the
Registry witness. The devnet rejected an altered preimage. Occupancy
per Cell fell by 113 CKB from the 267 CKB in-term reduction layout and
by 89 CKB from the earlier 243 CKB retirement-only layout.

For the earlier 175 plus 70 CKB comparison with two positions, replacing
their occupied Capacity yields 553 CKB locked and a 44.3% principal
fraction, compared with 731 CKB and 33.5% in the 243 CKB layout.
The new workload activated a 100 CKB genesis position and separate 75
and 70 CKB positions at the first handoff. Its three positions used
707 CKB of total Capacity and 245 CKB of principal, a 34.7% fraction.
The separate top-up position retained its own liability rather than
merging with the genesis position.

The two active validators moved from voting weights 495 and 400 to
360 and 400 at Stem height 21 after in-term requests for 65 and 70
CKB. The normal settlement repaid 65 CKB and kept 35 CKB active.
It returned 224 CKB to close the second position, including its recoverable
storage Capacity. That payout funded an 8 CKB position with the other
validator. The new position was confirmed on CKB at its activation
height of 31, and the online validators agreed on weights 360 and 408.

The historical run froze the validator for two votes at Stem height 3.
The same proof forfeited 65 CKB of pending principal from the genesis
position and failed against the position activated at height 11. The
later position recovered all 70 CKB and its occupied Capacity. Evidence
from height 22 also failed against the pending position excluded at
height 21. Ordinary retirement, in-term withdrawal and orphaned CKB
anchor rejection passed on the compact layout.

| Operation | Transaction bytes | Estimated CKB cycles |
| --- | ---: | ---: |
| Create the genesis position | 1,423 | 1,980,492 |
| Activate the 75 CKB position | 2,250 | 2,215,090 |
| Confirm the first reduction | 2,148 | 2,174,481 |
| Apply its historical penalty | 3,148 | 6,457,736 |
| Close the 70 CKB position | 1,491 | 2,047,761 |

The Rust release workspace passed 440 tests, with 12 ignored. The Go
proof suite passed and the comparison Stake Registry compiled for
RISC-V. The archive retains the initial failed 25 CKB partial payout
attempt. Its output was below the beneficiary Lock's occupied Capacity.
The later runs used 65 CKB.

[Reproduction](OPERATIONS.md#exercise-ckb-delegation) ·
[Raw evidence, fingerprints and failure](benchmarks/2026-11-25-stem-delegation-window.json.xz).

## Closed-epoch stake rewards and commission

Revision `eaece03`, compared with the compact delegation baseline
`47350a5`. The experiment used three local Stem replicas with changing
weighted membership and CKB 0.210.0 devnet. A confirmed CKB deposit
funded an ordinary signed payment with an 800 CKB fee at Stem height 7.
The fixture assigned 65% to validators and delegators. Epoch 0 closed
after height 10 with a 520 CKB budget. A finality proof and the epoch
handoff authenticated the Header at height 11 used for the reward path.
The next closed epoch was queried under the authenticated Header at
height 21 and had zero additional budget.

| Verified quote | CKB |
| --- | ---: |
| Validator, including commission | 389.99999999 |
| Commission included above, 1,250 basis points | 18.57142857 |
| Delegator, after commission | 130.00000001 |
| Total | 520.00000000 |

The historical Registry held 250 CKB of self-bond and a 100 CKB
delegation. The proof fixed both recipients' CKB Lock hashes. The CLI
rejected an altered principal for the historical position. CKB rejected
an invalid active-principal update path. Regression runs retained the
154 CKB occupied Capacity, forfeited 65 CKB from the older position, and
returned 70 CKB of principal and occupied Capacity to the later
position. A separate four-replica run reconciled all four existing
aggregate fund payouts on CKB at agreed Stem height 44. Individual
shares in this round are verified quotes.

| Operation on CKB devnet | Signed transaction bytes | Estimated CKB cycles |
| --- | ---: | ---: |
| Create the first delegation with a principal proof | 1,491 | 5,318,083 |
| Activate the 75 CKB position | 2,320 | 2,199,316 |
| Request the 65 CKB partial reduction | 1,738 | 7,853,834 |
| Confirm that reduction | 2,320 | 2,197,451 |
| Forfeit the pending principal with historical evidence | 3,116 | 6,399,762 |

The values come from `estimate_cycles` on the signed transactions
subsequently committed on devnet. Creation and in-term reduction add a
sparse update to authenticate the prior and successor principals.
Activation and the pending penalty keep the active principal root. The
prior round used a different Registry and checkpoint proof, so these
figures describe the tested fixtures.

The Rust release workspace passed 440 tests with 12 ignored. The Go
proof suite and the RISC-V comparison contract check passed. The
archive retains both initial failed compilations, due to a visibility
error in the shared sparse proof module, and all completed devnet runs.

[Reproduction](OPERATIONS.md#verify-closed-epoch-stake-rewards) ·
[Run evidence and build logs](benchmarks/2026-11-25-stem-stake-rewards.json.xz).

## Individual stake reward redemption

Revision `a4b1a81`, with the historical quote experiment at `7beaf30`
as the baseline. Three Stem replicas and CKB 0.210.0 devnet collected
800 CKB in bridged fees at Stem height 7. The fixture allocated 520 CKB
to the validators and delegators fund. One validator bonded 250 CKB,
and one delegator funded separate 40 and 60 CKB positions with the same
CKB Lock. Both positions counted in epoch 0. Its 1,250 basis-point
validator commission was 18.57142857 CKB.

| CKB recipient | Positions | CKB paid | Stem claim bytes | CKB release proof bytes | Estimated CKB release cycles |
| --- | ---: | ---: | ---: | ---: | ---: |
| Validator's Stake Cell owner | 1 | 389.99999999 | 569 | 996 | 1,934,804 |
| Delegator | 2 | 130.00000000 | 865 | 690 | 1,886,050 |

The 52 CKB share was below the beneficiary's 61 CKB minimum output.
Bundling it with the 78 CKB share produced one valid CKB output. The
successor validator finalized the claims at heights 30 and 36. The
vault used the same CKB checkpoint as the Stake Registry to verify both
handoffs and release both claims. The CKB releases were reconciled into
Stem at heights 34 and 40. All three replicas returned the same Header
at height 40. The numeric cycle costs are the CKB RPC estimates on the
transactions subsequently included on devnet.

Final liquid reserve was 27,748.00000001 CKB and circulating Capacity
was 27,468 CKB. Pending payouts were zero. Unallocated reserve was
280.00000001 CKB, which includes the other three fund allocations and
one shannon of rounding residue. Neither the bond nor the delegations'
principal funded the rewards.

The runner rejected a forged principal, wrong Lock, inflated amount,
below-minimum claim, aggregate validator-fund bypass and replays with
different transaction contents. Rebundling a position after confirmed
CKB payment was rejected. Direct CKB validation also rejected a changed
recipient, changed proof and missing checkpoint. A four-replica
control without staking reconciled all four fixed fund recipients at
agreed height 43.

The archive includes the first failed Rust build, the fixture import
error, the completed separate-checkpoint run and the final
shared-checkpoint run. The release workspace passed 441 tests with 12
ignored. The Go proof suite, CometBFT comparison build check and
RISC-V contract checks passed.

[Reproduction](OPERATIONS.md#verify-closed-epoch-stake-rewards) ·
[Raw evidence and run logs](benchmarks/2026-11-25-stem-stake-reward-claims.json.xz).

## Accrued stake reward balances

Revision `ebfd4b1`, compared with the bundled payout at `2822e9c`.
The same 800 CKB fee workload used three Stem replicas and CKB 0.210.0
devnet. The delegator's 52 CKB and 78 CKB shares accrued in two separate
transactions at Stem heights 30 and 32. The first share fell below the
61 CKB capacity required by the recipient Lock. Both accruals kept the
native lane's first withdrawal nonce unused and recorded zero pending
withdrawal liability. At height 32 the aggregate accrued liability was
130 CKB and free fee reserve was 670 CKB.

The runner killed the active archive validator after the second
accrual and reopened the same node home. It recovered finalized height
32 and independently authenticated the 130 CKB balance against a
finality proof and the committed Patricia path. An altered value was
rejected. A redemption using only the stale 52 CKB balance was also
rejected.

| Action | Stem height | Stem transaction bytes | CKB paid |
| --- | ---: | ---: | ---: |
| Accrue first delegation share | 30 | 652 | |
| Accrue second delegation share | 32 | 652 | |
| Claim and redeem validator share | 34 | 569 | 389.99999999 |
| Redeem accrued delegation balance | 40 | 109 | 130.00000000 |

The payout-only operation contained the CKB Lock and amount with an
empty epoch list. The bundled one-shot baseline used one 865-byte
transaction. Separating accrual and redemption used 1,413 bytes in
three transactions. Each accrual could commit on its own timetable.
The final delegator release used a 792-byte CKB proof and an estimated
1,897,986 CKB cycles. The validator transfer and delegate transfer were
reconciled at heights 38 and 44. All three replicas agreed at 44.

Final accrued and pending amounts were zero. The reserve was
27,748.00000001 CKB against 27,468 CKB in circulation, leaving
280.00000001 CKB unallocated. Replayed accruals and rebundled positions
after confirmed payment were rejected. Four-replica controls covered all
four fixed fund payouts and CKB and xUDT round trips with treasury
rotation and reserve movement. The release workspace passed 442 tests
with 12 ignored. The Go proof suite and CometBFT build check passed.

[Reproduction](OPERATIONS.md#verify-closed-epoch-stake-rewards) ·
[Raw accrual, recovery and regression evidence](benchmarks/2026-11-25-stem-stake-reward-accrual.json.xz).

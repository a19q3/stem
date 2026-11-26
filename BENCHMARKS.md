# Stem measurements

The [historical benchmark record](HISTORICAL-BENCHMARKS.md) and its raw artefacts describe commit `29872505350e931a252ddee9f88d94edf410c83a` or the earlier revisions identified in that record. Rates, latency and cycle measurements from those runs are baseline observations.

Stem measurements record the executable, workload, configured limits, offered and acknowledged submissions, successful finalizations and replica visibility.

The three-minute signed payment study compares two proposal intervals, two block capacity profiles and sender allocation on four local validators. Its archives replay to a certified Header and include sampled execution receipts. A separate pair of disposable CKB and Stem devnets exercised a published Lua Mutant from both asset origins. [Payment experiments](research/storage/proposal-cadence/README.md) · [On-disk cadence comparison](research/storage/proposal-cadence/nvme/README.md) · [Mutant lifecycle and proofs](research/spore-permit/upstream/beta2/README.md)

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
block preparation and sync-write timings. The newer
[replicated Lineage experiments](#sustained-replicated-lineage-and-amm-contention)
measure concurrent admission and sustained confirmation with both
shared and independent AMM pools.

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

## Shared CKB checkpoint at genesis

Revision `870d81f` enforces the deployment used in the `2351fe1`
stake reward experiment. The node derives the identity of the Stake
Registry from its complete CKB Script and verifies the guardian-signed
domain. Vault Scripts use the same policy decoder in Stem and on CKB.
The startup check compares both the checkpoint code hash and Type ID.

The CKB devnet deployed a second vault with the same network and
checkpoint executable but a different checkpoint Type ID. The vault
creation transaction was included on CKB. Stem rejected the genesis
that named this alternative vault. It also rejected a substituted
Registry Script under the original guardian observations and a mismatched
Stem chain ID. Workspace tests changed the code hash and Type ID
independently on the second lane.

| Valid configuration | Observed result |
| --- | --- |
| Shared checkpoint with stake rewards and retirement | Two committee handoffs. The outgoing validator's 250 CKB self-bond returned after retirement. The two reward payouts reconciled on all three replicas at Stem height 44. |
| Four-replica CKB and xUDT bridge | Actual CKB and xUDT payouts, reserve transfer and vault rotation. All replicas agreed at height 46. |
| Stake only with complete committee replacement | An observer restored the state at the CKB-anchored height 33 and verified agreement at height 35. |

The release workspace passed 442 tests with 12 ignored. The test that
opens genesis from real CKB guardian observations was run separately
and passed. The Go proof suite, CometBFT comparison build and RISC-V
contract checks passed.

The first contract build omitted the CKB-VM target flags and reached
an unsupported instruction during Registry creation. The successful
runs rebuilt with `-C passes=lower-atomic -C target-feature=-c`. The
archive preserves the failure, both sets of binary fingerprints, and
the confirmed substituted vault transaction.

[Genesis and deployment](OPERATIONS.md#start-a-native-replica) ·
[Raw experiment evidence](benchmarks/2026-11-25-stem-checkpoint-policy.json.xz).

## CKB Cell references in StemVM

Revision `6614033`, based on `35fe09c`. Four local Jolteon replicas and
CKB 0.210.0 devnet exercised an actual RISC-V Type Script using syscall
2130. The experiment bridged the script code into Stem through an
ordinary registered vault. The guardians certified CKB block 22, which
the Stem bridge adopted at height 9. Each reference used three guardian
signatures.

| Accepted reference | CKB output | Finalized Stem height | Signed transaction bytes | Receipt cycles |
| --- | --- | ---: | ---: | ---: |
| Live at the anchor | Consolidated vault output | 14 | 1,208 | 13,959,940 |
| Historical inclusion | Deposit output already spent by consolidation | 75 | 939 | 13,949,533 |

The receipt includes 12,000,000 cycles for the three signatures. The
remaining cycles include the ordinary payment Lock and the sample Type
Script. A live reference expired after the configured 64 Stem heights
from adoption. The historical reference then remained usable against
the same accepted CKB anchor. All four replicas agreed at height 76.

The fixture rejected a request to attest the spent deposit's liveness.
Stem rejected a missing reference, a historical record used where a
live observation was required, an altered guardian signature, a quorum
for a different CKB anchor and an expired live observation. On a
disposable CKB fork replacement, the guardian journal rejected further
observations. The ordinary CKB and xUDT bridge regression, including
reserve adjustment and vault rotation, passed at agreed Stem height 44.

The release workspace passed 442 tests with 12 ignored. The Go proof
suite and CometBFT comparison build check passed. The archive retains
the compilation error in the first sample contract, the harness errors
and the run that exposed the extra confirmation depth needed for the
mint's accepted anchor. The successful run records the exact
transactions, CKB inclusion evidence, guardian votes and execution
receipts. The live result applies to the specified anchor on the
guardian-observed chain.

[Reproduction](OPERATIONS.md#read-a-ckb-cell-from-stemvm) ·
[Raw runs and regression evidence](benchmarks/2026-11-26-stem-l1-references.json.xz).

## Independent CKB anchor refresh

Revision `0ac5a10` added a 373-byte control transaction with three
guardian signatures. Four Jolteon replicas finalized it at Stem
height 77, moving the accepted CKB anchor from block 22 to 23.
The proved vault lane, including its OutPoint and nonces, the backing
record and cumulative fees were identical before and after. The
control receipt charged zero user fee and zero VM cycles. Its bytes
and signatures consumed the separate anchor allowance.

The old live reference was expired before refresh, while a historical
reference still executed at height 75. A fresh live reference executed
at height 79 and charged 13,959,940 cycles in a 1,208-byte transaction.
All replicas agreed at height 80. Admission rejected a minority
certificate, a signature from the reference domain, a replayed
refresh and a reference to the superseded anchor. Guardians refused
the unconfirmed refresh and halted both signing flows after the CKB
fork replacement. A separate four-replica CKB and xUDT run completed
withdrawal, reserve movement and vault rotation at agreed height 44.

[Reproduction](OPERATIONS.md#read-a-ckb-cell-from-stemvm) ·
[Both refresh runs and bridge regression](benchmarks/2026-11-26-stem-anchor-refresh.json.xz).

## Delegation occupancy and both vote heights

Revision `1d24c13` reduced the CKB Delegation Cell data to 40 bytes.
The occupied Capacity measured on CKB 0.210.0 was 146 CKB per Cell,
down from 154 CKB in the previous compact layout and 243 CKB in the
original retirement-only layout. Applying the new layout to the earlier
175 and 70 CKB positions requires 537 CKB total lockup, with 45.6%
principal. The latest run used three positions of 100, 75 and 70 CKB.
They locked 683 CKB total, with 35.9% principal. The Registry holds
eight additional bytes per validator record for the earlier vote height.

The devnet accepted an actual QC vote from Stem height 3 and a signed
conflicting vote at height 4 for the same view. It rejected a freeze
record that substituted height 4 for the earlier height. The global
freeze blocked withdrawal of the 65 CKB pending reduction from the
genesis position, and the settlement forfeited that principal in a CKB
transaction. The position activated at height 11 rejected the evidence
and recovered its 70 CKB plus 146 CKB occupied Capacity. Evidence after
the cutoff at height 21 was also rejected. A separate run withdrew both
reductions normally and redelegated 8 CKB. The new position was
confirmed at Stem height 31. Another run rejected both the orphaned
anchor and its CKB replacement.

| Operation in the mixed-height run | Transaction bytes | CKB estimate_cycles |
| --- | ---: | ---: |
| Activate the 75 CKB position | 2,328 | 2,214,624 |
| Confirm the 65 CKB reduction | 2,328 | 2,184,864 |
| Freeze the validator using votes at heights 3 and 4 | 3,257 | 6,412,406 |
| Settle the 65 CKB penalty | 3,226 | 6,459,289 |
| Close the 70 CKB position | 1,577 | 2,053,372 |

The reward regression exercised validator retirement, closed-epoch
principal and commission proofs, accrual across a restart and confirmed
CKB payouts of 389.99999999 CKB to the validator and 130 CKB to the
delegation beneficiary. The release workspace passed 443 tests with 12
ignored. The Go proof suite and the CometBFT feature build passed. Both
native and comparison Stake Registry contracts cross-compiled for CKB-VM.

[Reproduction](OPERATIONS.md#exercise-ckb-delegation) ·
[Signed transactions, rejections and raw runs](benchmarks/2026-11-26-stem-delegation-liability.json.xz).

## Bidirectional Intercell Messaging

Revision `c9b0cc1` continued from `b0750bf` via `b13a38e`. Three
local rounds used four native Jolteon replicas and CKB 0.210.0.
The final round published the Stem execution checkpoint at height 20,
delivered two Stem outbox messages on CKB, and finalized one CKB
mailbox message plus its application Cell at Stem height 35. The CKB
message output had already been spent by a subsequent send when
guardians observed it. All four replicas agreed at height 38.

| Operation in the final round | Signed transaction bytes | Cycles |
| --- | ---: | ---: |
| Stem → CKB, first delivery | 1,390 | 4,196,661 CKB estimate |
| Stem → CKB, second delivery with a nonempty replay proof | 1,213 | 4,132,480 CKB estimate |
| CKB mailbox send for the accepted message | 820 | 1,682,035 CKB estimate |
| CKB → Stem, including three guardian signatures | 1,448 | 13,920,778 Stem receipt |

The Stem delivery paid 7,812 shannons. The first two rows include the
CKB transaction's Lock and mailbox verification. Finality checkpoint
publication is a separate on-chain transaction. This fixture measures
per-operation size and execution cost.

The final round rejected a different CKB genesis Header, a wrong owner
or receiver, a missing or alternate checkpoint, modified proof and
payload bytes, a forged guardian signature, and an untyped CKB output
certified by genuine guardians. CKB rejected the unchanged witness at
a mailbox configured for a different Stem network. Both chains rejected
duplicate consumption. The restarted Stem replica served the inbox
proof and rejected replay with a fresh input. A real CKB fork
replacement orphaned a message at an anchor that Stem had yet to adopt.
Stem rejected its certificate and the guardian journal stopped signing
for the orphaned anchor. The prior committed inbox record was still
available.

The archive includes all three message rounds, signed and rejected
transactions, proof and checkpoint data, build fingerprints, and both
rounds of CKB/xUDT, reference and stake/reward regressions. The release
workspace passed 445 tests with 12 ignored. The Go proof suite and
CometBFT comparison feature check passed.

[Reproduction](OPERATIONS.md#exercise-intercell-messaging) ·
[Raw runs, signed transactions and rejection evidence](benchmarks/2026-11-26-stem-intercell.json.xz).

## Stem-native UDT settlement

Revision `30a562f` deploys the RISC-V Stem UDT and escrow programs
through the real CKB bridge. The fixture runs four Jolteon replicas
and CKB 0.210.0. It issues 1,000 application UDT units and exports
250 through Intercell. CKB verifies the checkpoint, mints 250
mapped units and permits a separately signed transfer.

In the partial return, the new owner burns 150 CKB mapped units.
Stem releases 150 original units and leaves 100 in escrow for the
100 CKB-side units still circulating. The full return burns all 250
mapped units, releases all 250 originals and leaves just the fixed
100 CKB bond in the Stem escrow. The CKB mailbox also retains its
bond while an independent input pays relay fees.

| Operation | Partial return bytes | Partial cycles | Full return bytes | Full cycles |
| --- | ---: | ---: | ---: | ---: |
| Stem export | 1,109 | 2,012,504 | 1,109 | 2,012,504 |
| CKB mapped mint | 2,187 | 4,288,686 | 1,983 | 4,243,635 |
| CKB mapped transfer | 526 | 1,663,904 | 526 | 1,665,256 |
| CKB mapped burn | 1,542 | 3,423,593 | 1,441 | 3,351,028 |
| Stem redemption | 2,252 | 14,007,294 | 2,131 | 14,004,073 |

CKB cycles are estimates from the devnet before submission. Stem cycles
come from the committed execution receipts. Stem redemption includes
12,000,000 cycles for three guardian signatures. Checkpoint publication
and the CKB anchor update are separate transactions.

The fixture rejects unbacked, inflated or redirected CKB minting,
replayed mint and redemption, missing or insufficient burns, arbitrary
export nonces, escrow bond theft, redirection on return and export of an
asset Cell carrying a second Type Script. The ordinary CKB/xUDT bridge
and bidirectional Intercell regressions passed. The Rust workspace passed
446 tests with 12 ignored. The Go proof suite passed. The archive
includes the initial compiler error, successive contract fingerprints,
all successful devnet runs and signed rejection cases.

[Reproduction](OPERATIONS.md#exercise-stem-native-udt-settlement) ·
[Raw reports, proofs and rejections](benchmarks/2026-11-26-stem-udt-settlement.json.xz).

## CKB Spore V2 settlement through Stem

Revision `d08fefa` ran three settlement rounds on four Jolteon replicas
with CKB 0.210.0. The fixture deployed the published Spore V2 mainnet
ELF with data hash `4a4dce1d…b9f5` on the local CKB chain. It minted a
real Spore, locked the original in CKB custody, issued its Stem
representation from a confirmed CKB output, transferred it to a
different Stem owner and burned it. The final CKB transfer released
the original Spore with its exact Type Script, original ID, data and
360 CKB capacity. The native endpoint retained a separate 100 CKB
bond. The final round then redeposited that original and reissued the
same representation Type Script at Stem height 33.

| Operation in the final round | Signed bytes | Cycles |
| --- | ---: | ---: |
| CKB original mint | 899 | 1,787,710 CKB estimate |
| CKB original into custody | 2,223 | 3,501,380 CKB estimate |
| Stem import | 2,203 | 14,050,692 Stem receipt |
| Stem ownership transfer | 415 | 1,941,880 Stem receipt |
| Stem return with burn | 877 | 2,055,722 Stem receipt |
| CKB original release | 2,710 | 4,383,142 CKB estimate |
| CKB redeposit | 2,223 | 3,538,299 CKB estimate |
| Stem reimport | 2,203 | 14,050,692 Stem receipt |

The three guardian signatures account for 12,000,000 cycles of each
Stem import. The CKB Spore V2 Type Script itself rejected a transfer
without its CoBuild witness. The fixture also rejected content or
recipient substitution, fake issuance, duplicate mapped outputs,
repeat imports, premature burns, missing checkpoints, attempts to
take the original Cell's capacity and an earlier return proof
submitted against the newly redeposited Spore. All 16 final rejection
cases appear in the raw report.

The final four replicas agreed at height 33. Native UDT full return,
CKB/xUDT payout and generic Intercell regressions passed. The Rust
workspace passed 447 tests with 12 ignored. The Go proof suite passed.
The measured asset used plain MIME content with no Cluster. The tested
lifecycle begins with an original CKB Spore.

[Reproduction](OPERATIONS.md#exercise-ckb-spore-settlement) ·
[Raw runs, signed rejections and fingerprints](benchmarks/2026-11-26-stem-spore-settlement.json.xz).

## CKB Spore V2 with Cluster membership

Four Jolteon replicas and CKB 0.210.0 ran the complete custody lifecycle
with the published Spore and Cluster ELFs. The original Cell carried
360 CKB. Its 99-byte Molecule data carried the same Cluster ID
at CKB issuance, deposit, Stem ownership transfer, release and redeposit.
CKB transferred ownership of the actual Cluster while its member was
on Stem. That transfer preserved Cluster data and had no effect on the
member's identity or redemption.

| Operation | Plain Spore, candidate bytes / cycles | Cluster member, candidate bytes / cycles |
| --- | ---: | ---: |
| CKB member issuance | 899 / 1,730,905 | 1,081 / 1,809,943 |
| CKB deposit | 2,223 / 3,529,890 | 2,295 / 3,564,515 |
| Stem import | 2,203 / 14,050,931 | 2,311 / 14,052,410 |
| Stem ownership transfer | 415 / 1,942,041 | 451 / 1,943,329 |
| Stem return | 877 / 2,055,996 | 913 / 2,057,457 |
| CKB release | 2,612 / 4,375,553 | 2,684 / 4,394,858 |
| CKB redeposit | 2,223 / 3,526,182 | 2,295 / 3,557,085 |
| Stem reimport | 2,203 / 14,050,931 | 2,311 / 14,052,410 |

CKB cycles are node estimates for signed transactions. Stem cycles are
from committed receipts. The 36 membership bytes raised the calculated
occupied Capacity from 189 to 225 CKB under the owner's Lock and from
297 to 333 CKB under escrow. The separate CKB Cluster creation and
transfer used 819 and 1,025 transaction bytes and 1,742,896 and
1,744,863 estimated cycles.
The baseline and candidate plain runs both completed, with identical
byte counts for the eight operations above. The baseline refused a valid
clustered deposit after native CKB minting. The archive also retains an
earlier clustered attempt where the fixture omitted an input from the
two-input CKB Sighash Lock signing group.

The final clustered run recorded 22 rejection controls, including native
minting without the Cluster dependency or owner authorization, identity
forgery and Cluster removal at the custody boundary, loss of membership
on Stem, and a repeated redemption after redeposit. Independent CKB
verification checked six outputs against transaction and witness roots
and rejected an altered release witness. The WASM SDK verified the
Cluster bytes in the live mapping, absence of the burned mapping, the
Intercell inbox and outbox, and all four Stem execution receipts under
Jolteon finality. This profile covers native Spore V2 transfer semantics
for the two plain MIME types. Application extensions require their own
verification rules.

The dedicated Stem object route, shared Metatype return, generic Intercell,
CKB/xUDT withdrawals and funded Jolteon handoff passed devnet regression
runs. The Rust workspace passed 494 tests with 22 ignored. The Go proof
suite and release `comet-benchmark` build check passed.

[Reproduction](OPERATIONS.md#exercise-ckb-spore-settlement) ·
[Signed runs, proofs, controls and build fingerprints](benchmarks/2026-11-26-stem-spore-cluster.json.xz).

## Native compound object settlement over CKB

Revision `df181fd` tested a Stem-issued object with two actual Type
Groups on four Jolteon replicas and CKB 0.210.0. The original spent
into escrow with its identity, separate approval Type Script, immutable
data and 200 CKB capacity. A finalized export drove the CKB issuance
of one mapped object. The new owner burned the mapping after a CKB
transfer. The confirmed burn allowed Stem to restore the original
under the requested recipient with its two Type Scripts and contents
intact. The Stem escrow bond remained 120 CKB. The final replicas
agreed at height 27.

| Operation in the final run | Signed bytes | Cycles |
| --- | ---: | ---: |
| Stem native issue with independent approval | 1,215 | 3,897,492 Stem receipt |
| Stem export with independent approval | 1,708 | 3,991,191 Stem receipt |
| CKB mapped issue | 2,179 | 4,250,919 CKB estimate |
| CKB transfer with two independent Locks | 886 | 3,281,177 CKB estimate |
| CKB mapped burn | 1,523 | 3,408,568 CKB estimate |
| Stem restoration | 2,555 | 14,063,850 Stem receipt |

The final fixture rejected 15 invalid transitions, including a forged
native identity, removal of the independent policy, missing approval
on both chains, changed recipient or content, messages addressed
to reserved custody Locks, and replay after the CKB representation
had already been burned. All three object runs are archived.
The first used a 250 CKB mapped output. CKB rejected it because this
fixture's occupied capacity was 256 CKB. The two successful rounds
used 360 CKB.

CKB/xUDT payout and generic Intercell regressions passed. Rebuilt
native UDT and CKB Spore routes passed the same remote-custody
recipient rejection checks in both directions, alongside their
normal settlement paths. The Rust workspace passed 448 tests with 12
ignored. The Go proof suite passed.

[Reproduction](OPERATIONS.md#exercise-stem-native-object-settlement) ·
[Full runs, signed rejections, and build fingerprints](benchmarks/2026-11-26-stem-object-settlement.json.xz).

## Sparse CKB checkpoint publication

The stake fixture ran two CKB-backed committee changes on three Stem
replicas and CKB 0.210.0. A fresh publisher process opened the same journal
on each poll. It published the two required handoffs, honored the routine
interval while idle, anchored a requested Header at its exact height, and
later published by interval alone. A CKB reorganization detached the first
checkpoint before confirmation. The publisher resubmitted the same signed
transaction and confirmed it on the replacement branch. Reorganizing a
confirmed checkpoint halted further publication.

| Fixture and publication | Stem height | Signed CKB bytes | CKB estimated cycles |
| --- | ---: | ---: | ---: |
| Stake, first handoff | 10 | 2,396 | 6,210,846 |
| Stake, second handoff | 20 | 2,284 | 6,196,365 |
| Stake, on-demand | 25 | 1,951 | 6,104,412 |
| Stake, scheduled | 28 | 1,919 | 6,048,835 |
| Four-replica Intercell, creation | 1 | 2,311 | 14,414,374 |
| Four-replica Intercell, outbox proof | 20 | 2,331 | 14,311,201 |

All recorded publications were mined on the disposable CKB chain and
reached two further canonical blocks before the journal advanced. In
the Intercell fixture, the published Header authenticated both saved
outbox proofs. The mailbox consumed both messages. Stem authenticated
the CKB return message, rejected replay after restart and rejected the
message sourced from an orphaned CKB block. The four replicas agreed
at Stem height 41. The fixture exercised 20 rejection cases.

An earlier Intercell run advanced to the latest Header on demand.
CKB confirmed that checkpoint, but the saved Patricia proof was for an
earlier Header. Stem no longer served the needed historical state path
when asked after CKB confirmation. The exact-height publication test
uses the proof collected before publication. The archive retains that
failed run, two initial replica port collisions, signed transactions,
canonical inclusion proofs and reproduction commands.

CKB and xUDT payouts, delegated staking and reward redemption, and
historical and live-at-anchor Cell references passed their devnet
regressions. The Rust workspace passed 448 tests with 12 ignored. The Go
proof suite passed.

[Reproduction](OPERATIONS.md#run-a-sparse-ckb-checkpoint-publisher) ·
[All runs, signed transactions and build fingerprints](benchmarks/2026-11-26-stem-checkpoint-publisher.json.xz).

## Consensus-time Keeper

Two four-process Jolteon runs executed an exact-height reservation at
height 32 and a timed reservation at height 53 with real RISC-V
Lineage authorization. The second run authenticated the pending agenda
and command body, restarted one validator while the command waited,
then verified the same execution and receipt proofs on all four
replicas. The parent Header crossed the requested timestamp by 15 ms,
and all replicas could serve the receipt 3.857 seconds after the
restart began. That interval includes time spent waiting for the signed
trigger. It does not isolate restart catch-up latency.

The storage fixture held the same target and genesis policy constant.
It alternated an empty agenda and a full sleeping agenda across 31
blocks in each condition. The funded agenda occupied 1,668 bytes for
32 entries and rejected a 33rd reservation. It then crossed the
timestamp in a proposal and drained the state over eight blocks of four
successful commands. The threshold-crossing proposal itself executed
none of the commands. Times below come from the final single-process
local run.

| Agenda condition | Blocks | Prepare p50 / p95 / p99 | Sync commit p50 / p95 / p99 |
| --- | ---: | ---: | ---: |
| Empty | 31 | 7.4 / 13.3 / 15.8 µs | 0.518 / 0.577 / 1.516 ms |
| 32 sleeping | 31 | 14.8 / 24.6 / 24.6 µs | 0.503 / 0.615 / 0.658 ms |
| Four applied per block | 8 | 2.496 / 2.647 / 2.647 ms | 0.649 / 0.716 / 0.716 ms |

Each active block charged 15,481,672 cycles for the four attempts.
The initial benchmark configuration reserved all eight transaction slots
and was rejected at genesis. The measured run reserved two exact-height
slots and four timed slots, leaving capacity for submitted transactions.
The raw archive retains the rejected setup and both measured runs.
These measurements cover local block preparation and synchronous writes.

The workspace passed 449 tests with 13 ignored. The four-replica fixture
and storage benchmark ran separately. The Go proof suite and the frozen
CometBFT feature check passed. The CKB devnet round exercised
bidirectional Intercell with the current native transaction schema.

[Reproduction](OPERATIONS.md#exercise-the-native-jolteon-path) ·
[Raw timings, proofs and the rejected configuration](benchmarks/2026-11-26-stem-keeper-time.json.xz).

## CKB checkpoint fees from the PoW Security Fund

Four Jolteon replicas and CKB 0.210.0 exercised the complete fee path.
Research-network payments extinguished 4,000 CKB of Stem liabilities.
All four revenue accounts were reconciled through five actual CKB
payouts. The two PoW fund payouts carried 200 CKB each. A merge retained
all 400 CKB and paid its own fee from a separate wallet input.

The funded update moved the checkpoint from Stem height 46 to 56.
The authenticated Stem time advanced 3,200 ms against the fixture's
3,000 ms interval. CKB confirmed the transaction with unchanged
checkpoint capacity, a 0.01 CKB transaction fee and 399.99 CKB under
the original fund Lock. The Stem backing record stayed identical
after CKB spent the already-paid fund. All four replicas agreed at
Stem height 59.

| Same checkpoint proof, different fee source | CKB transaction bytes | CKB estimated cycles |
| --- | ---: | ---: |
| Independent Sighash wallet | 2,363 | 14,317,078 |
| Restricted PoW fund | 2,394 | 12,752,078 |

These are CKB node estimates against the same live checkpoint and
witness. Only the funded transaction was submitted. The paired run
saved 1,565,000 cycles with the fund Lock.

A separate three-replica CKB-backed committee test funded a real
handoff from Stem height 9 to 10, using a CKB contribution to the
same restricted Lock. Its parent and successor timestamps were
760 ms apart, inside the configured four-hour ordinary interval.
The handoff took 2,217 CKB transaction bytes and an estimated
4,580,268 cycles. The publisher recovered its pending fund transaction
after restart and used the original wallet change at height 20.
The fund change remained a live CKB Cell.

The bridge fixture exercised nine rejections, including early
publication, excessive fees, merged principal shortfall, changed
recipients, extra outputs, a forged checkpoint and a foreign Type ID.
Two genesis checks rejected use as an ordinary withdrawal recipient
and use with a foreign checkpoint. The first devnet stopped at genesis
because the ordinary recipient rules excluded the custom Lock. A
later malformed allowlist test failed at JSON parsing. Both attempts
are archived alongside the verified runs.

Unrestricted fee payouts, ordinary publisher recovery and CKB/xUDT
bridge devnets passed their regressions. The Rust workspace passed
450 tests with 13 ignored. The Go proof suite passed.

[Reproduction](OPERATIONS.md#fund-ckb-checkpoint-fees) ·
[Raw measurements, transactions, proofs and failed attempts](benchmarks/2026-11-26-stem-checkpoint-fee-fund.json.xz).

## Paid CKB application security domain

The final integration fixture ran the RISC-V security domain contract
on CKB 0.210.0. Four independently keyed service positions locked 800
CKB of principal and 498 CKB of occupied capacity. The 75% application
quorum used the original pledged denominator. Two successful requests
each paid 600 CKB to the actual signers and issued a typed receipt with
194 CKB of buyer-controlled capacity. CKB rejected a false Live Cell
result with a valid service quorum and denied use of the active bond
as capital in another domain.

Two objective double reports caused CKB to transfer 180 and 100 CKB
of collateral to the configured penalty address. The second penalty
executed during retirement and restarted the relative 12-block
challenge period for the remaining capital. After maturity, CKB
returned 520 CKB to the two remaining positions and released the
498 CKB reserve. A pending request recovered its full buyer escrow
after domain closure. The test also verified that the first receipt
owner could recover its capacity without reissuing the receipt.

| Canonical CKB operation | Signed bytes | CKB estimated cycles |
| --- | ---: | ---: |
| Register four bonded positions | 986 | 2,248,924 |
| Serve the first request with three signers | 1,930 | 8,935,881 |
| Serve after the first penalty | 1,930 | 8,807,290 |
| Verify two conflicting signatures and slash | 1,365 | 6,405,416 |
| Slash during retirement | 1,365 | 6,377,009 |
| Close after the challenge period | 788 | 2,308,265 |
| Refund the pending request | 570 | 1,707,829 |

Four Jolteon replicas finalized the Stem application transaction at
height 37. The transaction used a historical CKB reference to the
already spent first receipt and a RISC-V Type Script that required
the exact record. It used 970 bytes and 13,953,027 receipt cycles.
The guardian rejected the spent receipt as a live reference, while
the application rejected both a missing and a substituted record.
The final fixture ran 17 CKB or provider rejection checks and three
Stem or guardian rejection checks.

The archive retains both successful and failed runs. One early
negative test accidentally redirected a principal payment to its
existing destination because the penalty address was also a member
beneficiary. A distinct penalty address and a different output
destination made the final test exercise the intended rejection.
The records also include compiler diagnostics, actual payout
transactions, and canonical CKB inclusion proofs.

The Rust workspace passed 451 tests with 13 ignored. The Go proof
suite, bidirectional Intercell devnet, and CKB-backed Jolteon
committee and penalty regressions passed.

[Reproduction](OPERATIONS.md#exercise-a-paid-ckb-security-domain) ·
[Raw transactions, proof data, measurements, and unsuccessful attempts](benchmarks/2026-11-26-stem-security-domain.json.xz).

## Intercell mailbox recovery after CKB reorganization

Revision `9020349` extended the bidirectional fixture originally started
from `b0750bf`. Two successful rounds used four Jolteon replicas and a
real CKB 0.210.0 devnet on one i9-13900H host. The last round published a
Jolteon checkpoint for Stem height 20. Stem consumed an authenticated CKB
message at height 37, and all four replicas agreed at height 40. The
original [bidirectional runs](#bidirectional-intercell-messaging) remain
available.

The last round built both CKB delivery witnesses from chain history with
an empty local relay state. The second CKB delivery used a Mailbox input
and output at index 1, alongside an independent input and output at
index 0. One fork first committed that delivery. Truncating and
replacing its CKB block made the orphaned head unavailable. Recovery
from its surviving predecessor returned the first consumed ID and
regenerated the same second witness. A new transaction then delivered
that message on the replacement chain. The Mailbox still rejected a
replay. A separate CKB reorganization removed a send before Stem
accepted its anchor. The guardian journal refused to sign again, and
the committed Stem inbox survived a replica restart.

| Last round, recovered CKB Mailbox | Transaction reads | Consumed IDs | CLI wall time |
| --- | ---: | ---: | ---: |
| After creation | 2 | 0 | 11.14 ms |
| After first delivery | 3 | 1 | 12.36 ms |
| Before replacing the second delivery's block | 5 | 2 | 14.56 ms |
| From the surviving head after replacement | 3 | 1 | 12.10 ms |
| After both deliveries and two sends | 7 | 2 | 16.06 ms |

Times include process startup and local RPC. Each row is a single
sample. The bounded scan reads the mailbox history and earlier
unrelated inputs needed to locate the real witness. This fixture does
not measure long-history recovery. The two successful CKB deliveries
were 1,390 and 1,663 bytes, estimated at 4,202,078 and 5,876,835 CKB
cycles. The second included an extra independent Lock, so its cost
cannot isolate the replay path. The Stem receive was 1,448 bytes,
including three guardian signatures.

The final message fixture recorded 24 expected rejections, including a
foreign Mailbox Type Script, exhausted recovery budget, orphaned
heads, forged proofs, wrong networks and replay attempts. The first
attempt failed on the `0x` prefix in normal CKB RPC hashes. Both final
runs use the same prefix-aware parser as the existing CKB CLI. The
archive preserves that attempt and the initial build failure.

The release workspace passed 451 tests with 13 ignored. The Go proof
suite and the frozen CometBFT comparison feature check passed.
Independent CKB rounds verified CKB/xUDT redemption, historical and
live-at-anchor references, the four fee fund payouts, and CKB-backed
delegation, slashing and stake reward redemption.

[Reproduction](OPERATIONS.md#exercise-intercell-messaging) ·
[Raw reports, commands and fingerprints](benchmarks/2026-11-26-stem-intercell-recovery.json.xz).

## Native OTX Action authorization

Revision `c3208af` exercised independently signed pairs and one
sponsor on four native Jolteon replicas on an i9-13900H host. Each
participant signed the canonical Action manifest along with the exact
pair and shared dependencies. The solver assembled four pairs with a
different pair order. The application Type group's index moved from
1 in the signing fragment to 4 in the complete transaction. The group
received the same signed `open` Action through syscall 2123. All four
replicas verified the same execution receipt and finality proof.

| Four-replica fixture | Stem height | Signed bytes | Charged VM cycles |
| --- | ---: | ---: | ---: |
| Sponsored two-asset swap | 7 | 1,135 | 5,886,419 |
| Sponsored two-asset swap with Actions and a fourth pair | 9 | 1,497 | 7,837,364 |

The second fixture also executes a fourth Lock and an additional Type
Script. Its size and cycles measure the whole transaction. These rows
cannot isolate the cost of the Action manifest. Both swaps pay the
same test fee, 5,000,000,000 shannons.

Each replica rejected the transaction with an altered Lock Action
payload. The local RISC-V test also rejected changed Action order and
group identity, added and omitted Actions, and an unrelated proof.
An otherwise valid collection of pairs without the required Action
was rejected by the actual Type Script. The wallet rejected a
reserved Intercell Action, while the regular full-authorization path
passed a separate CKB devnet round with four Jolteon replicas.
Serial and parallel execution produced identical Headers and receipt
cycles. Real Polytype, Lineage and Keeper execution continued through
a restart in the same four-replica fixture.

The first four-replica run rejected a duplicate development genesis
allocation. The rerun used separate test OutPoints. The archive also
preserves initial build and shared target directory failures. The
release workspace passed 452 tests with 13 ignored. The explicit
four-replica test, Go proof suite and frozen CometBFT feature check
passed.

[Wallet workflow](OPERATIONS.md#wallet-and-proofs) ·
[Raw proofs, runs, failures and commands](benchmarks/2026-11-26-stem-otx-actions.json.xz).

## Atomic Lineage batch settlement

Revision `4ea694e` combined two independently signed bilateral
settlements, each with its own sponsor and two RISC-V asset Type
Groups. The solver reviewed and assembled a shared ordered plan using
the wallet CLI. Three runs with four native Jolteon replicas on one
i9-13900H host finalized the batch at Stem heights 29, 30 and 29. In
each run all four replicas verified the same receipt and finality
proof. All four Lineage Head proofs showed version 1 after the existing
Keeper fixture restarted a replica. The on-chain receipt recorded
4,000 shannons and 15,471,432 VM cycles for a 1,633-byte envelope.

| Matched fixture in one local Store | Wire bytes | Charged VM cycles |
| --- | ---: | ---: |
| Two separately authorized commands | 1,534 | 15,471,406 |
| One atomic batch of the same commands | 1,633 | 15,471,406 |

The local comparison used the same initial state, signed fee amounts,
Executors and settlement payloads in each run. The batch added 99
bytes and used the same charged cycles in that fixture. This measures
transaction encoding and protocol charges. It does not measure
throughput or wall time for batching.

The four-replica admission test rejected a jointly signed batch whose
second Executor returned an error. The local proposer then admitted a
valid batch reusing the same fee Cells and targets after rejecting the
failed batch in the same selection overlay. A separate RISC-V Type
Script vetoed a late update, and a 12 million aggregate cycle limit
rejected the batch. The tests also rejected individually submitted
members, reordered or substituted members, duplicate fee inputs,
overlapping targets, nested batches and altered seals. Serial and
parallel replay produced the same Header and aggregate receipt.

The archive preserves all three network rounds, signed plans,
transaction and Head proofs, failures, commands and binary
fingerprints. The Rust release workspace passed 454 tests with 13
ignored. The explicit four-replica test, Go proof suite and CometBFT
comparison feature check passed. Bidirectional Intercell Messaging
passed the real CKB devnet regression.

[Wallet workflow](OPERATIONS.md#wallet-and-proofs) ·
[Original evidence and reproduction commands](benchmarks/2026-11-26-stem-lineage-batch.json.xz).

## Keeper scheduling for atomic Lineage batches

Revision `cf44f9f` exercised two independent sponsors and four reserve
Heads in each batch. Two rounds used four native Jolteon replicas on
one i9-13900H host. Both rounds ran a single command and a batch in the
same height bucket, then a single command and a batch with the same
time trigger. The test used a disposable 512-read block limit to
admit a two-command batch alongside the other reserved work.

| Trigger | Finalized wake heights across the two runs | Signed bytes | Fee | Charged VM cycles |
| --- | ---: | ---: | ---: | ---: |
| Height | 44, 44 | 1,651 | 4,000 shannons | 15,471,432 |
| Committed parent time | 80, 81 | 1,651 | 4,000 shannons | 15,471,432 |

Each sponsor's authenticated reservation record was 77 bytes.
Finalized proofs showed those records before execution and their
absence afterward. The parent Headers of the timed wakeups were
59 ms and 143 ms after the target timestamp. The fixture restarted
one replica during the waiting period and verified the same wake
receipt and version-3 Heads on all four nodes. These are deterministic
receipt charges and two trigger observations, with no throughput
measurement.

Local serial and parallel execution matched after snapshot restore.
A failing second Executor made the timed batch skip with zero
execution fee and kept both sponsor Cells live. It charged the
42,857,142-cycle reservation share under the test genesis. A lower
share rejected the aggregate authorization and VM budgets at
registration. The secondary sponsor canceled a batch, both sponsor
links were cleared, and an old cancellation was rejected after the
primary fee Cell was attached to a replacement reservation. The tests
also rejected a split command, a mismatching trigger, a forged
second-sponsor seal and reuse of a secondary fee in a separate queue
entry.

The Rust release workspace passed 455 tests with 13 ignored. The
explicit four-replica test, Go proof suite and CometBFT comparison
feature check passed. Bidirectional Intercell Messaging passed a real
CKB devnet regression.

[Wallet workflow](OPERATIONS.md#wallet-and-proofs) ·
[Both replica rounds, failures and reproduction commands](benchmarks/2026-11-26-stem-keeper-batch.json.xz).

## Prepaid bounded Keeper retries

Revision `5e54460` exercised a signed three-attempt policy with an
eight-block delay, two sponsors and four reserve Heads. Two runs used
four native Jolteon replicas on one i9-13900H host. The Keeper carrier
paid 8,000 shannons in the fixture, including 6,000 for reservations.
The RISC-V Executor of the second member rejected the first attempt
until its signed minimum height. A replica restarted between attempts.

| Attempt in both runs | Finalized height | Signed bytes | Execution fee | Charged cycles |
| --- | ---: | ---: | ---: | ---: |
| First, recorded as waiting for retry | 93 | 1,667 | 0 shannons | 42,857,142 reserved |
| Second, applied | 101 | 1,667 | 4,000 shannons | 15,472,003 |

All replicas verified identical transaction bytes across attempts and
the same receipt for each height. The 66-byte agenda record proved the
next height and two remaining attempts after the first failure. The
successful retry advanced all four Heads and cleared both sponsor
reservations and the unused attempt. The first row measures charged
slot capacity. It does not measure the RISC-V work before rollback.

The local test exercised serial and parallel execution after snapshot
restore, time-triggered exhaustion of a standalone job, cancellation
and early termination after a secondary sponsor Cell was spent. It
rejected an underfunded carrier, an invalid attempt count, divergent
member policies and an expiry window too short for the signed
interval. Successful execution fees were charged once. The fixture
also checked that wallet JSON and the Rust API produced identical
retry drafts.

The Rust release workspace passed 456 tests with 13 ignored. The
explicit four-replica test, Go proof suite and CometBFT comparison
feature check passed. Bidirectional Intercell Messaging passed a real
CKB devnet regression. Sustained replicated Keeper throughput was
outside these runs.

[Wallet workflow](OPERATIONS.md#wallet-and-proofs) ·
[Both replica rounds, failures and reproduction commands](benchmarks/2026-11-26-stem-keeper-retry.json.xz).

## Sustained Keeper load and admission expiry

Revision `92f173f` ran native Jolteon on one Intel i9-13900H host
with 20 logical CPUs. Each replica used two VM workers, a 250 ms
proposal interval, a 300-million-cycle block budget, and two slots
each for exact-height and timed Keeper work. The mixed workload ran
for 30 seconds and observed an additional 18 seconds of drain.
It combined independent height bookings, one-shot time bookings and
time bookings whose signed commands were designed to fail twice.
Ordinary payments ran concurrently at 24 per second. The observer
polled all replicas and used the minimum finalized height. The
reported visibility latency includes the polling interval of about
100 ms.

| Replicas and offered bookings/s | Payments sent / admitted / visible | Bookings sent / admitted / visible | Visible payments/s and bookings/s during offer window | Applied / Retrying / Skipped |
| --- | ---: | ---: | ---: | ---: |
| 4 at 6 | 720 / 720 / 720 | 180 / 180 / 180 | 23.67 and 5.93 | 135 / 45 / 45 |
| 16 at 6 | 720 / 720 / 720 | 180 / 180 / 180 | 23.57 and 5.90 | 135 / 45 / 45 |
| 4 at 12 | 720 / 720 / 720 | 360 / 271 / 271 | 23.60 and 8.90 | 209 / 62 / 62 |

All offered requests reached the RPC endpoint. The pressure run
reached the 32-entry timed agenda cap and rejected 89 bookings.
The two matched runs peaked at 14 entries. Over the full 48-second
observation window, payment and booking visibility rates were
15.00 and 3.75/s in each matched run and 15.00 and 5.65/s under
pressure. The successful Keeper attempts charged 3,870,418 cycles
each. A retry or terminal skip reserved a 60-million-cycle share
and charged zero execution fee. The test checked every Keeper
receipt proof, a representative receipt on every replica, and
the final common Header against the Jolteon proof.

| Replicas and offered bookings/s | Payment visibility p50/p95/p99 | Booking visibility p50/p95/p99 | Booking submission to successful wake p50/p95/p99 |
| --- | ---: | ---: | ---: |
| 4 at 6 | 497 / 633 / 664 ms | 488 / 627 / 658 ms | 3,091 / 3,295 / 3,313 ms |
| 16 at 6 | 508 / 651 / 679 ms | 522 / 672 / 697 ms | 3,133 / 3,339 / 3,361 ms |
| 4 at 12 | 488 / 627 / 658 ms | 497 / 763 / 1,204 ms | 4,636 / 6,390 / 6,571 ms |

| Replicas and offered bookings/s | Total replica CPU | Final replica RSS sum | Process write bytes | Peer transport bytes sent |
| --- | ---: | ---: | ---: | ---: |
| 4 at 6 | 7.04 s | 161.7 MiB | 165.05 MB | 3.02 MB |
| 16 at 6 | 42.15 s | 662.4 MiB | 525.30 MB | 42.08 MB |
| 4 at 12 | 8.54 s | 180.2 MiB | 171.23 MB | 3.41 MB |

Process write bytes come from Linux process I/O accounting, and
network counters measure application frames before TCP overhead.
The replicas shared CPU, storage and loopback. These runs measure
Keeper scheduling and slot pressure at the stated workload.

A separate fixed-height contention pair offered 48 bookings for
two slots while ordinary payments ran. Both admitted 13 bookings
and executed two. With expiry disconnected, all four replicas
retained 11 obsolete carriers, or 7,898 bytes per replica, after
drain. With expiry connected, all four pools were empty. The
paired control changed only the post-finality pruning call. The
first smoke run failed because its node command passed an
unsupported flag. The archive includes that run and the failing
control alongside the corrected runs.

The final binary passed 456 release workspace tests with 14 ignored,
plus the Go proof suite and frozen CometBFT build check. Real CKB
devnet regressions passed for bidirectional Intercell with 24
expected rejections, CKB and xUDT payout, historical and live-at-anchor
references, stake retirement and penalties, stake rewards, and all
four fee fund payouts. The Intercell run published the exact outbox
Header at height 21, recovered the canonical CKB inbox after the
delivery reorg, and reached common Stem height 45 after restart.

[Reproduction](OPERATIONS.md#wallet-and-proofs) ·
[Raw RPC observations, proofs, controls and regressions](benchmarks/2026-11-26-stem-keeper-load.json.xz).

## CKB checkpoint publisher handoff

Revision `01df462` ran two publishers with separate fee keys and
journals on a real CKB 0.210.0 devnet. Three Jolteon replicas ran the
CKB Stake Registry fixture on one i9-13900H host. It included two
committee handoffs. The final run used two confirming CKB blocks
after inclusion.

Publisher A bootstrapped at Stem height 10 and submitted the second
handoff at height 20. CKB rejected publisher B's competing transaction
under its fee replacement policy. B then adopted the confirmed spend,
published the height-25 checkpoint with its independently funded
Cell and adopted its own confirmed pending transaction after a new CLI
invocation. A adopted B's checkpoint. A manually constructed the
height-26 update with the checkpoint input, output and witness at
index 1 and added 1 CKB to its Capacity. B adopted this update, then
published height 27 from that OutPoint while preserving the additional
Capacity and spending its own earlier change. A fresh journal adopted
all four predecessors in one invocation from the pinned seed.

| Final run | Stem height | Checkpoint witness bytes | Estimated CKB-VM cycles |
| --- | ---: | ---: | ---: |
| A creates checkpoint and publishes first handoff | 10 | 1,398 | 6,231,684 |
| A publishes second handoff | 20 | 1,218 | 6,226,380 |
| B publishes with its own wallet | 25 | 885 | 6,112,987 |
| B publishes after the index-1 successor | 27 | 819 | 6,052,125 |

B took 15.6 ms of local CLI wall time to adopt the winning spend.
The fresh journal verified and adopted four updates in 13.7 ms.
Each figure is a single local sample including process startup,
RPC and journal I/O. CKB mining and confirmation wait time are
outside these samples.

The final round recorded nine expected failures. These covered
shallow depth, a valid foreign checkpoint, a skipped handoff, a
missing bootstrap, unknown or unrelated transactions, the competing
spend, and both ordinary publication and adoption after the confirmed
anchor was reorganized. A sequence with a valid prefix and an unknown
tail left the journal unchanged, as verified by the subsequent adoption.
The preceding attempted run checked the foreign Type ID before reaching
the required depth. Its early depth rejection and the initial
successful run are preserved alongside the corrected run.

The existing single-publisher shallow reorg and retry, funded
committee handoff, and four-replica bidirectional Intercell devnet
fixtures passed. The release workspace passed 456 tests with 14
ignored. Go proof tests and the CometBFT comparison build passed.

[Reproduction](OPERATIONS.md#run-a-sparse-ckb-checkpoint-publisher) ·
[Signed transactions, inclusion proofs, rejection traces and commands](benchmarks/2026-11-26-stem-checkpoint-adoption.json.xz).

## 128-member checkpoint contention on CKB

Revision `2e3a375` ran 128 weighted Jolteon replicas, finalized 32
signed payments, and verified their common Header at height 12.
It saved ordinary two-chain proofs for exact finalized heights 10,
11 and 12. Two CKB 0.210.0 devnet nodes on the same i9-13900H host
verified those proofs in the deployed RISC-V contract. They retained
the default pool settings of 70,000,000 cycles for queue classification
and 1,000 shannons/kB minimum fee rate.

Both CKB nodes began at the same confirmed height-10 checkpoint.
With P2P disconnected, each pool independently admitted a different
height-11 spend of the same OutPoint. The two spends used separate
fee keys and the same certified execution Header. The origin
mined one spend and reached the required depth. After reconnecting,
the receiving node rejected its own pending spend because the shared
CKB input was dead. Its publisher adopted the winning checkpoint
and published height 12 using its original funding Cell. Both nodes
returned identical CKB witness inclusion proofs.

| Devnet block cycles | Relayed transactions | Verified pool cycles per node | Transaction counts by inclusion block | Publisher command to remote pending |
| ---: | ---: | ---: | --- | ---: |
| 10,000,000,000 | 1 | 357,906,133 | 1 | 747 ms |
| 10,000,000,000 | 5 | 1,792,235,186 | 5 | 879 ms |
| 10,000,000,000 | 9 | 3,226,531,188 | 9 | 1,033 ms |
| 10,000,000,000 | 17 | 6,095,114,209 | 17 | 1,376 ms |
| 3,500,000,000 | 9 | 3,226,531,188 | 9 | 1,083 ms |
| 3,500,000,000 | 17 | 6,095,114,209 | 9 then 8 | 1,382 ms |

Each burst included one publisher follow-up and independently
funded background checkpoint creations with distinct Type IDs.
The secondary CKB node received the background transactions over
P2P, and the origin received the follow-up over P2P. The publisher
timing includes proof construction, cycle estimation, the journal,
submission, and poll-based observation of the remote pool. It ends
before mining. With the 3.5-billion budget, the follow-up was in
the first block. The same 17 transactions' pending-pool footprint
was 305,754 bytes per node. Across these measured bursts, the two
CKB processes used 0.65 to 13.79 CPU seconds in aggregate between
the beginning of the burst and complete remote visibility. The
10-billion 17-transaction repeat observed two entries in the peer's
verification queue at one sample. The 3.5-billion experiment
demonstrated pending inclusion backlog across two blocks.

These are single-host bursts using an actual fixed 128-member
committee, with the archive replica reopened to construct and
serve the publication witnesses. The next experiment measures a
[CKB-funded full-size epoch handoff](#ckb-funded-128-member-jolteon-handoff).
Sustained CKB queue pressure remains to be measured. The standalone
ordinary proof relay, 456 Rust release tests, Go proof tests and
the CometBFT comparison build passed.

[Reproduction](OPERATIONS.md#exercise-the-native-jolteon-path) ·
[Raw runs, competing transactions, inclusion proofs and resource samples](benchmarks/2026-11-26-stem-ckb-checkpoint-contention.json.xz).

## CKB-funded 128-member Jolteon handoff

Revision `9f74f9e` exercised a 128-to-128 change on a single
i9-13900H host with 129 Stem processes and CKB 0.210.0 devnet.
The devnet used a 3.5-billion-cycle block budget. The run deployed
fresh RISC-V builds of the Stake Registry and Jolteon checkpoint.
Each validator used a distinct consensus key and a separate CKB
Stake Cell funded by the fixture wallet.

Genesis locked 22,080 CKB of voting principal across 128 Stake
Cells. The fixture confirmed 256 Registry transitions for
registration and activation. Before the epoch-1 snapshot, one
150-CKB validator requested exit, a 255-CKB validator registered
and activated, and one continuing validator added 40 CKB.
Registry sequence 260 committed 129 records, including the
exiting Stake Cell. The successor's 128 voters had 22,225 total
power. Three guardians independently opened the confirmed L1
records and the native snapshot was 22,767 bytes.

The snapshot was admitted at a reported Stem tip of height 11.
The outgoing committee certified the boundary at height 40
with 87 and 86 votes in the two certificates, representing
14,820 and 14,850 power out of 22,080. The new committee's
height-42 proof carried 85 and 86 votes, representing 14,845
and 14,935 power out of 22,225. One continuing voter stopped
at height 3 before submission. On restart it showed the
committed snapshot and the authenticated height-40 boundary.
All 129 processes agreed on the Header at height 44.

| CKB checkpoint | Stem height | Witness bytes | Estimated CKB-VM cycles |
| --- | ---: | ---: | ---: |
| Incumbent execution proof | 10 | 16,994 | 406,869,793 |
| Epoch handoff | 40 | 22,492 | 410,671,305 |
| Successor execution proof | 42 | 16,750 | 406,047,056 |

CKB mined each publication and returned its transaction and witness
inclusion proof. The contract rejected the valid successor proof
against the old checkpoint when the handoff was skipped, and
rejected replay of the already applied handoff. The Stem verifier
also rejected the successor proof when the handoff was omitted.
Genesis rejected the two-signature guardian sample.

In this single wide run, initial Stake Cell registration and
activation took 103.83 s. Guardian observation took 0.42 s.
Admission to successor visibility among running replicas took
17.03 s. Restart to common height 44 took 0.96 s. The four-member
fixture used the same height and proof checks. Its paused voter
represented a larger share of the schedule, so these timing
samples do not isolate the cost of committee width.

The first four- and 128-member runs reached the successor epoch
and recovered the paused node. Their test driver stopped because
it required an increment in the replay fast-path counter. The
corrected driver verifies the recovered transaction, epoch and
certified boundary. Both first runs and both successful reruns
are preserved. The existing delegated-stake, active-slash and
retirement devnet regression passed with the rebuilt contracts.
Rust release tests passed 456 with 14 ignored, and the Go proof
suite and CometBFT comparison build passed.

This round used one CKB node to publish the full-size boundary.
A separate [two-peer replay](#128-member-epoch-boundary-relayed-over-ckb-p2p)
used the same signed boundary and successor transactions. The
wide rotation here replaced one key and changed one continuing
member's weight.

[Reproduction](OPERATIONS.md#exercise-a-full-funded-committee-handoff) ·
[Raw runs, signed Stake Cell transactions, quorums and CKB proofs](benchmarks/2026-11-26-stem-funded-wide-handoff.json.xz).

## 128-member epoch boundary relayed over CKB P2P

Revision `917d62f` took the exact signed boundary and successor
transactions from the funded 128-to-128 handoff. It opened a
disconnected copy of the original CKB devnet, verified the saved
transaction and witness proofs, and independently imported its
canonical prefix through CKB height 991 into two fresh nodes.
Only the old committee's checkpoint was present at that height.
The original boundary and successor were both unknown to the two
test nodes at the start of their respective trials. The final
prefix import took 6.56 seconds on the same i9-13900H host.

Both fresh CKB 0.210.0 nodes used the 3.5-billion-cycle block budget
and default pool settings. The origin alone received each RPC
submission. The other node observed each transaction as pending
over their direct P2P link and then mined it. Both nodes returned
the same inclusion proofs and exact packed transactions. The new
CKB blocks had distinct hashes from the source chain's blocks.
Each accepted transaction consumed its predecessor and left the
expected checkpoint as a live output.

| Final run | Stem height | Witness bytes | Committed CKB-VM cycles on each peer | RPC submission to remote pending | RPC submission to remote inclusion |
| --- | ---: | ---: | ---: | ---: | ---: |
| Incumbent-to-successor handoff | 40 | 22,492 | 410,671,305 | 672 ms | 983 ms |
| Successor execution proof | 42 | 16,750 | 406,047,056 | 585 ms | 894 ms |

Both peers' recorded execution cycles matched the pre-submission
estimate. The other instrumented 128-member run measured 733 ms
and 581 ms to remote pending. A four-member control consumed
14,620,319 and 14,476,058 committed cycles. These are single-host,
sequential samples. The fixture once again rejected the valid
successor witness against the old checkpoint when the handoff
was skipped.

The first trial stopped after trying to reuse an HTTP connection
whose startup request had failed. The second trial applied the
full-size cycle assertion to the four-member control. The corrected
runner resets the failed connection and applies the threshold only
to the full-size test case. The archive retains both attempts,
the successful runs, and the standalone ordinary and contention
regressions. The Rust release workspace passed 456 tests with 14
ignored. The Go proof suite and CometBFT comparison build passed.

[Reproduction](OPERATIONS.md#exercise-a-full-funded-committee-handoff) ·
[Raw runs, resource samples, signed transactions and inclusion proofs](benchmarks/2026-11-26-stem-wide-epoch-p2p.json.xz).

## Automatic CKB checkpoint successor discovery

Revision `3b932ed` tested the optional `auto_follow` publisher
mode on CKB 0.210.0 and a three-process Stem network. The local
CKB Indexer found candidates by the exact checkpoint Type Script.
The existing verifier independently checked their canonical
inclusion height, confirmation depth, predecessor, checkpoint
proof and committee handoff before updating the journal.

The losing publisher returned `awaiting_successor` before the
winning spend reached two CKB confirmations. It then adopted
the winner and published with its own fee input. When its own
pending transaction reached the chain, the usual
`awaiting_confirmations` and `confirmed` results included its
wallet funding information. A fresh journal discovered five
updates across two committee handoffs. Two dependent updates
landed in CKB block 66. After pinning the first transaction,
the next automatic poll correctly found the later position
in that same block. The valid unrelated checkpoint used a
distinct Type ID, so discovery did not pick it up. Both manual
and automatic polling halted after the pinned confirmed
CKB block was replaced.

A separately funded publisher then submitted 65 advances
from the existing checkpoint. Automatic recovery applied 64
updates on the first poll and one on the second. The original
publisher then confirmed a new update at Stem height 94 using
its unchanged, separately tracked fee input.

| Final run | Verified updates | Resulting Stem height | Local CLI wall time |
| --- | ---: | ---: | ---: |
| Recover competing spend | 1 | 20 | 17.4 ms |
| Follow from inside the same CKB block | 1 | 27 | 19.2 ms |
| Follow from a fresh journal | 5 | 27 | 24.2 ms |
| First batch of the 65-update backlog | 64 | 92 | 89.6 ms |
| Second batch | 1 | 93 | 20.1 ms |

These are single-host invocation times including process startup,
RPC, proof verification and journal I/O. They start after the
respective CKB blocks reached the required depth.

The first long-history fixture tried to reduce a checkpoint
from 1,001 to 1,000 CKB. CKB rejected it. The rerun inherited
the live predecessor's Capacity and completed all 65 advances.
The archive includes the failed trial, signed transactions,
Indexer responses summarized by the publisher, and confirmed
inclusion proofs. The manual-adoption, single-publisher reorg,
funded-handoff, and bidirectional Intercell regressions passed.
The Rust release workspace passed 456 tests with 14 ignored,
and the Go proof suite and CometBFT comparison build passed.

[Reproduction](OPERATIONS.md#run-a-sparse-ckb-checkpoint-publisher) ·
[Raw runs, included transactions and journal outcomes](benchmarks/2026-11-26-stem-publisher-auto-follow.json.xz).

## Intercell delivery after checkpoint advancement

Revision `11d7c01` adds CKB verification of an older Stem Outbox proof
under the MMR of a later Jolteon checkpoint. Both rounds ran four Stem
replicas and a CKB 0.210.0 devnet on one i9-13900H. The fixture saved
two Outbox proofs, delivered the first message, added a third message
to change the SystemRoot, then advanced the CKB checkpoint. The second
message was delivered using its original Patricia proof and the new
history path.

| Checkpoint publication | Outbox → checkpoint height | Current proof B / cycles | Historical proof B / cycles | Agreed Stem height |
| --- | ---: | ---: | ---: | ---: |
| Direct CLI bundle | 21 → 26 | 1,565 / 5,860,883 | 1,973 / 5,912,701 | 50 |
| Sparse publisher | 22 → 28 | 1,667 / 5,854,474 | 2,075 / 5,902,348 | 49 |

Sizes cover the signed CKB transaction. Each pair uses the same
transaction body, current Mailbox replay path, and CKB checkpoint.
The control uses a fresh Patricia path for the same message. CKB
`estimate_cycles` reports an increase of 47,874–51,818 cycles and both
historical transactions add 408 bytes. Each historical transaction
was committed and its CKB transaction and witness inclusion checked.
The controls were estimated only. These two pairs do not measure
long-gap or cross-epoch history paths.

Both rounds recorded 29 expected rejections, including a stale Outbox
path without history, a path built for the wrong checkpoint, and
changed historical Header, frontier and sibling bytes. They also
verified wrong-network rejection, one-time consumption, CKB reorg and
redelivery, guardian rejection of an orphaned anchor, and Stem replica
recovery. Separate CKB devnet regressions covered CKB and xUDT
withdrawals, native UDT full return, original Spore return and
redeposit, native compound object return, historical and live-at-anchor
CKB references, fee fund payouts and CKB-backed stake and rewards.
The release workspace passed 457 tests with 14 ignored. The Go proof
suite and frozen CometBFT comparison feature check passed.

[Reproduction](OPERATIONS.md#exercise-intercell-messaging) ·
[Raw proofs, rejections, commands and binary fingerprints](benchmarks/2026-11-26-stem-intercell-history.json.xz).

## Portable CKB security-domain accounting

Revision `708434d` exports CKB transaction and witness proofs from the
domain's Type ID creation through a selected Cell or closure. The
offline verifier uses independently supplied CKB block hashes. The
functional fixture runs the deployed RISC-V security-domain program
on CKB 0.210.0. One mode also consumes the service receipt from four
live Jolteon replicas. Both modes were repeated after tightening
evidence file publication.

The final standalone round verified seven endpoints. The original
800 CKB of principal reconciled to 280 CKB slashed and 520 CKB returned.
Two service requests paid a separate total of 1,200 CKB to their
signers. The audits matched the actual per-member CKB outputs and
excluded a refunded request. Retiring the domain reduced eligible
capital to zero while 620 CKB was still locked. A second penalty
during the challenge window reduced locked capital to 520 CKB.

| Final standalone endpoint | Proven CKB transactions | Proof JSON bytes | Online collection | Offline verification |
| --- | ---: | ---: | ---: | ---: |
| Creation | 1 | 2,955 | 17.83 ms | 5.61 ms |
| First service | 2 | 7,797 | 18.39 ms | 6.23 ms |
| Live after second service | 4 | 16,351 | 14.10 ms | 3.74 ms |
| Closure | 7 | 25,657 | 22.18 ms | 4.42 ms |

The two timings include process startup. Collection uses localhost
RPC. These are single samples on an i9-13900H, with no inference about
long-running domains. The final four-replica round confirmed receipt
use at Stem height 41 and verified its seven audit endpoints.

The standalone round rejected ten invalid audit attempts. These include
omitted history, a false target endpoint, modified witness bytes, the
wrong code or genesis, a consumed Cell requested as live, insufficient
confirmations, exhausted collection budget and existing proof files.
After a real CKB reorganization the historical closure proof failed
against the replacement chain's hashes. The separate Intercell and
CKB-backed stake regressions passed. The final Rust workspace passed
457 tests with 14 ignored. The Go proof suite and frozen CometBFT
comparison check passed.

[Reproduction and trust assumptions](OPERATIONS.md#verify-security-domain-capital-and-income) ·
[All runs, public evidence, commands and fingerprints](benchmarks/2026-11-26-stem-security-domain-audit.json.xz).

## Authenticated per-vault fee attribution

Revision `f4c8c49` was tested on CKB 0.210.0 devnet with four native
Jolteon replicas on one i9-13900H. The final CKB/xUDT round committed
two fee-paying burns totaling 2,000,000 shannons. The first burn
used a parent with native-only reserves. Before the second burn,
recognized reserves were 4,599,201,000,000 shannons in the native
lane and 120,000,000,000 in the xUDT lane.

| Final round | Vault allocations | Real CKB movements verified |
| --- | --- | --- |
| CKB and xUDT | 1,974,571 shannons native and 25,429 xUDT-lane shannons | Both withdrawals, 500,000-shannon reserve transfer, custody rotation |
| Four ordinary fund payouts | 800 CKB gross to the native lane | 800 CKB paid from that lane |
| PoW checkpoint expense fixture | 4,000 CKB gross to the native lane | 4,000 CKB fund payments, including two PoW funding outputs and checkpoint fee spends |

The allocation record remained identical through reserve transfer and
custody rotation, and across each fund payout sequence. The two
Patricia paths and Strand record in the final dual-vault round were
481, 583 and 74 bytes, respectively, before JSON encoding. All eight
queried Headers in the three final rounds were independently verified
using `stem jolteon-proof`. The verifier rejected a substituted
network or global tally. The Rust test also rejected a redistribution
that kept the total unchanged, checked serial and parallel first
mint plus same-block payment, and verified the record after state
snapshot restoration.

The initial and final rounds and an initial fixture compilation failure
are preserved. The release workspace passed 458 tests with 14 ignored.
Go proof tests and the frozen CometBFT feature check passed. Separate
Intercell and CKB stake reward devnet regressions passed. These runs
measure records and two fixed lane configurations. Large lane
catalogues and their effect on sustained fee-paying throughput remain
unmeasured.

[Verification workflow](OPERATIONS.md#verify-per-vault-revenue) ·
[Raw rounds, finality proofs, failures and build fingerprints](benchmarks/2026-11-26-stem-vault-revenue.json.xz).

## Browser WASM transaction and proof SDK

Revision `c0ec9ac` built the native ledger and portable Jolteon verifier
for `wasm32-unknown-unknown`. Headless Chrome 157 and Node.js 22.23.1 ran
the same fixture against public proofs captured from four native
Jolteon replicas, a real CKB Intercell mailbox, and a separate
CKB-backed stake run. The stake evidence covers two committee handoffs
and a successor proof at height 25.

The test independently rebuilt a 413-byte transaction with a real
Intercell Group Action. The browser and native wallet produced equal
unsigned bytes and complete Lock digest. Inserting the actual
signature reproduced the finalized signed transaction byte for byte.
The verified receipt measured 1,920,520 VM cycles and a
1,000,000-shannon fee. The SDK also verified the original Live Cell
records, spent-Cell absence, the Outbox payload and 3,000,000 shannons
of proved vault revenue. Each JavaScript runtime rejected twelve
negative cases covering proof substitution, signed data, trust anchors
and replayed or missing handoffs.

| Observable | Final result |
| --- | ---: |
| Generated WASM binary | 636,453 bytes |
| Local npm package | 219,992 bytes compressed |
| Node.js initialization, one run | 3.44 ms |
| Node.js checks, one run | 25.37 ms |
| Node.js checks from unpacked npm package | 25.08 ms |

These are local process observations on an i9-13900H. The checks include
codec, state and finality verification plus negative cases. The Chrome
run was a functional test with virtual time and yields no timing
comparison. TypeScript 5.9.3 checked the generated declarations in
strict mode, including the disposable symbol library.

The first JavaScript fixture reused the same JSON key for finality
and a state path. The first type check omitted the compiler library
required by the generated disposal declarations. Both failed attempts
are archived with the corrected run. The locked Rust release workspace
passed 458 tests with 14 ignored. The Go proof suite and frozen
CometBFT feature check passed.

[Build and reproduce](OPERATIONS.md#exercise-the-browser-sdk) ·
[Proof inputs, browser output, commands and failures](benchmarks/2026-11-26-stem-wasm-sdk.json.xz).

## Sustained native payments and durable write attribution

Revision `53ba682` exposes exact serialized WriteBatch bytes by
key family and by synchronous operation. Revision `40f3451` reuses
a previously synchronized proposal body when the full encoded bytes
match. Each run used four local Jolteon replicas on an i9-13900H,
with two VM workers per node, a 250 ms proposal interval, a
2-billion-cycle cap, three 64-block retention nodes and one archive.
Payments used independently funded inputs and real RISC-V Lock
validation. The restored Store verified the authenticated roots with
restore writes accounted separately.

| Planned TPS, offer duration, mode | Actual offered TPS | Admission TPS in window | All-replica visible TPS in window | Visible after drain | Client drops | Visibility p50 / p95 / p99 (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 400, 45 s, metrics off | 400.00 | 399.87 | 393.47 | 18,000 | 0 | 0.583 / 0.731 / 0.761 |
| 400, 45 s, measured baseline | 400.00 | 400.00 | 395.78 | 18,000 | 0 | 0.584 / 0.733 / 0.761 |
| 400, 45 s, reused bodies | 400.00 | 400.00 | 396.18 | 18,000 | 0 | 0.579 / 0.725 / 0.751 |
| 1,200, 35 s, measured baseline | 1,155.26 | 1,155.09 | 1,095.37 | 40,434 | 1,566 | 1.693 / 2.363 / 2.519 |
| 1,200, 35 s, reused bodies | 1,167.51 | 1,166.83 | 1,102.51 | 40,863 | 1,137 | 1.636 / 2.237 / 2.358 |
| 2,200, 30 s, reused bodies | 1,736.47 | 1,735.67 | 1,073.53 | 52,094 | 13,906 | 2.513 / 33.820 / 39.226 |

The two 400 TPS measurement controls and the optimization used identical
pre-funded genesis inputs and signed transactions. The 1,200 TPS pair
also used identical inputs. These are single runs at each setting.
The near-400 TPS pair ended with 190 and 172 admitted transactions
waiting for visibility at the offer cutoff. The high-pressure run ended
the offer window with 19,864 still waiting. It reached 1,195.61
visible TPS when the denominator includes drain through the final
observed payment. Client pacing and node processing both constrain the
2,200-target run. Common height was sampled at approximately 50 ms
intervals and the archive's original transaction IDs were matched
against each timed submission. All replicas agreed on the final Header
and its Jolteon proof verified independently.

| 400 TPS with measurement, summed across replicas | Baseline | Reused bodies |
| --- | ---: | ---: |
| Proposal body WriteBatch bytes | 54.12 MB | 18.05 MB |
| Proposal bodies within finality writes | 36.10 MB | 0.017 MB |
| Live Cell Patricia nodes | 160.31 MB | 160.05 MB |
| Execution records | 18.45 MB | 18.45 MB |
| Live Cell records | 16.42 MB | 16.42 MB |
| Headers and history | 0.290 MB | 0.286 MB |
| RocksDB WAL bytes | 251.42 MB | 215.07 MB |
| Linux process write bytes | 489.78 MB | 441.06 MB |
| Replica CPU seconds | 53.00 | 53.21 |
| Peak sampled RSS | 567.45 MB | 566.82 MB |
| P2P application-frame bytes sent / received | 30.57 / 35.24 MB | 30.79 / 35.46 MB |

WAL bytes dropped 14.5% and process writes dropped 9.9% in this
400 TPS pair. These are submit-and-drain deltas. The 2,200-target
run used 159.90 CPU seconds, 918.21 MB sampled peak RSS, 923.03 MB
process writes and 121.98 / 135.38 MB of application-frame network
traffic. Its finality synchronous-write p99 fell in the
16,384–32,767 microsecond bucket.

RocksDB's flush ticker returned zero with this build while its
table-file creation events identified 65.28 MB and 70.41 MB of
flush outputs in the two measured 400 TPS runs, including process
startup. The 2,200-target run created 320.68 MB of flush outputs
over its process lifetime. No compaction output was observed in
these sampled lifetimes. WAL and SSTs use the shared atomic database.
The isolated history-index process wrote 944 filesystem output units
for the optimized 400 TPS run and retained 61,440 bytes of allocated
files. The raw archive retains per-node counters and SST creation
events separately from Linux process totals.

The release workspace passed 460 tests with 15 ignored. The Go proof
suite and frozen CometBFT feature check passed. Four replicas recovered
after one was offline for 200 blocks, with a 64-block retention
window. The bidirectional Intercell and CKB-backed stake and reward
fixtures passed. The first regression attempts encountered local
address conflicts and passed on different ports. Those attempts,
the initial compilation failures and the successful smoke run are
included in the archive.

[Run the fixture and inspect the storage query](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw timing samples, signed workloads and regression logs](benchmarks/2026-11-26-stem-storage-load.json.xz).

## Intercell delivery with multiple CKB references

Revision `4fc9181` used four Jolteon replicas and CKB 0.210.0 in
each round. It sent 4,096-byte payloads in both directions and rejected
4,097 bytes at the originating chain. CKB consumed Stem nonces 41 and
8 in that order. The second delivery used the original Outbox proof
after checkpoint advancement. Two reorgs per round exercised
redelivery on a replacement CKB chain and rejection of a guardian
certificate for an unaccepted anchor. The restarted Stem replica
served the committed inbox proof and rejected a new transaction
trying to consume the same message.

| Checkpoint publication | Older Outbox → checkpoint height | 4 KiB CKB send B / cycles | 4 KiB CKB delivery B / cycles | Stem receive B / cycles | Agreed Stem height |
| --- | ---: | ---: | ---: | ---: | ---: |
| Direct | 20 → 25 | 4,905 / 1,677,290 | 6,157 / 6,197,880 | 19,337 / 37,925,251 | 44 |
| Sparse publisher | 21 → 26 | 4,905 / 1,684,053 | 6,157 / 6,253,246 | 19,337 / 37,925,251 | 47 |

Sizes use the signed transactions. The CKB runner recorded
`estimate_cycles` and verified the committed transaction and witness
paths. Stem cycles come from the execution receipt. Each Stem
receive used nine guardian signatures over three CKB references. The
matching historical output had already been spent. A different
historical output from the same Mailbox and a live untyped output with
copied message bytes could accompany delivery. Providing only those
two references was rejected. Each round recorded 33 expected
rejections. These are functional and per-operation cost samples.

The first pair of runs hit CKB occupied Capacity on the fixture's
untyped output. The second pair hit the initial 20-million-cycle
Stem cap when verifying multiple certificates. The final runs funded
that fixed test output and set a 50-million-cycle transaction cap in
the generic Intercell test genesis. The archive includes each attempt.

Separate CKB devnet regressions passed for CKB and xUDT redemption,
four fee fund payouts, both kinds of L1 Cell Reference, and CKB-backed
stake handoffs and rewards. The locked Rust release workspace passed
460 tests with 15 ignored. The Go proof suite and frozen CometBFT
feature check passed.

[Reproduction](OPERATIONS.md#exercise-intercell-messaging) ·
[Signed transactions, reference certificates, proofs and failed runs](benchmarks/2026-11-26-stem-intercell-composition.json.xz).

## Intercell delivery across funded committee handoffs

Revision `1b07157` exercised 4, 16 and 128 active Jolteon voters
with one additional incoming process per run. Each devnet funded its
validators through the CKB Stake Registry and minted the sender's
Stem Cells against CKB reserves. It saved an Outbox proof in epoch 0.
At height 40 the new committee replaced one validator and changed a
continuing validator's weight. A paused continuing voter caught up
across the boundary. A second message in epoch 1 changed the Outbox
proof. All Stem replicas agreed on the later checkpoint Header.

| Committee | Original Outbox → later checkpoint | Fresh proof signed B / estimated cycles | Historical proof signed B / committed cycles | CKB P2P arrival / receiving inclusion |
| ---: | ---: | ---: | ---: | ---: |
| 4 | 10 → 49 | 1,607 / 4,227,101 | 1,977 / 4,306,532 | 172.8 / 482.7 ms |
| 16 | 9 → 49 | 1,403 / 4,181,189 | 1,875 / 4,278,288 | 168.4 / 475.8 ms |
| 128 | 9 → 54 | 1,403 / 4,184,125 | 1,907 / 4,294,913 | 254.1 / 564.4 ms |

Each row pairs different witnesses over the same CKB transaction body.
The historical delivery keeps the earlier Patricia path. It adds the
old execution Header and an MMR inclusion path under the successor
checkpoint. The fresh controls were estimated. The signed historical
transactions committed on the original devnet and again on two
independently initialized CKB peers. Those peers imported only the
prefix through the first checkpoint, then accepted the boundary, both
successor checkpoints and the historical delivery as new transactions.
Only the sending node submitted them by RPC. The receiving node
reported pending transactions received over P2P, mined them, and both
nodes verified the original signed bytes, witness inclusion and
recorded cycles. These are single-host devnet samples.

The 128-member boundary consumed 408,833,487 cycles. The later
checkpoint at height 54 consumed 408,471,906 cycles. Both entered the
receiving peer's stock pool over P2P with a 70-million-cycle transaction
verification setting and a 3.5-billion-cycle block limit. Those costs
are separate from mailbox consumption. The mailbox authenticates its
message with the already validated checkpoint Cell.

All three runs rejected a successor that skipped the handoff, an old
Outbox path without its history proof, and a repeat against the live
Mailbox. The relay rebuilt the consumed message ID from canonical CKB
history. An independent four-voter non-Intercell handoff and relay
control passed, as did the four-replica bidirectional fixture with 33
expected rejections. The locked Rust release workspace passed 460
tests with 15 ignored, and the Go proof suite passed.

[Reproduction](OPERATIONS.md#exercise-a-full-funded-committee-handoff) ·
[Raw CKB spends, witnesses, P2P traces, rejected attempts and commands](benchmarks/2026-11-26-stem-intercell-epoch.json.xz).

## Intercell sparse replay proofs and authenticated recovery

Revision `6678971` revalidated bidirectional delivery on CKB 0.210.0
and four local Jolteon replicas. Both the direct and sparse-publisher
rounds delivered a 4,096-byte message in each direction and passed 39
expected rejection checks. Each round orphaned a CKB consumption,
restarted the CKB node using the same data directory, and restored
the previous live Mailbox root. The recovered ID list and a 64-byte
sparse path produced identical delivery witnesses. CKB accepted the
redelivery with the sparse-path witness. The builder rejected a
truncated path, a stale root and a request supplying both inputs.

| Checkpoint publication | Outbox → checkpoint height | 4 KiB CKB delivery signed B / estimated cycles | Stem receive signed B / receipt cycles | Agreed height |
| --- | ---: | ---: | ---: | ---: |
| Direct | 20 → 26 | 6,157 / 6,162,676 | 19,337 / 37,925,251 | 50 |
| Sparse publisher | 21 → 28 | 5,953 / 6,123,764 | 19,337 / 37,925,251 | 52 |

These are per-operation samples with different Header history paths.
After a Stem replica restarted, the fixture advanced the accepted CKB
anchor and certified the same historical send at the new anchor.
Stem rejected the stale certificate and then rejected the fresh
certificate as a replay. A later CKB fork orphaned a send at an
unaccepted anchor. The guardian journal stopped signing and the
committed Inbox proof remained available.

The recovery control changed one byte of a committed CKB Lock
signature in the RPC response while forwarding the real Header,
transaction proof and witness proof. The previous `be502b0` CLI
returned the expected replay root with those mismatched bytes.
The new recovery command rejected them before reading the delivery
witness. The archive includes the failed control and both signed byte
sequences. The original unmodified baseline also passed its existing
bidirectional fixture.

Separate devnets revalidated CKB and xUDT redemption, vault rotation,
all four fee fund payouts, historical and live-at-anchor CKB
references, funded committee handoff with an older Outbox proof
delivered after the epoch boundary, and staking with delegation,
retirement, slashing and reward accounting. The direct fixture also
passed all 39 rejection checks on a clean rebuild of the committed
revision. The locked Rust release workspace passed 464 tests with 18
ignored. The Go proof suite and frozen CometBFT feature check passed.

[Reproduction](OPERATIONS.md#exercise-intercell-messaging) ·
[Raw devnet outputs, proofs, signed transactions, commands and build fingerprints](benchmarks/2026-11-26-stem-intercell-revalidation.json.xz).

## Sustained replicated Lineage and AMM contention

Revision `3d861a0` reuses the timed submission, storage and network
counters from the [sustained payment fixture](#sustained-native-payments-and-durable-write-attribution).
Each node ran on the same i9-13900H host, with a 250 ms proposal
interval, two VM workers unless stated, a 2-billion-cycle block cap
and 8,192 logical Cell reads per block. Three ordinary replicas and
one archive ran in the four-replica tests. The 16-replica tests used
one archive and 15 ordinary replicas. All ordinary replicas kept 64
blocks. Registrations finished before the offer window.

The AMM comparison starts from identical four-replica genesis state.
Each transaction uses a different funded fee Cell, changes the pool
and trader together, and runs both asset Type Groups. One workload
uses a single pool shared by all 32 trader reserves. The other uses
32 disjoint pool and trader pairs. The one-worker comparison reuses
the exact signed distributed workload. The 16-replica pressure run
reuses the signed four-replica hotspot workload. Counter fixtures
cover independent Heads, a mix of shared and disjoint Heads, and an
atomic group of eight Heads.

| Replicas, workload, planned TPS and offer window | Actual offer / admission / common visibility TPS in window | Offered / client drops | Visible by end of drain | Visibility p50/p95/p99 in seconds |
| --- | ---: | ---: | ---: | ---: |
| 4, 32 AMM pools, 240, 30 s | 237.13 / 236.83 / 218.77 | 7,114 / 86 | 7,114 | 2.000 / 2.606 / 2.746 |
| 4, one AMM pool, 240, 30 s | 234.43 / 234.37 / 217.37 | 7,033 / 167 | 7,033 | 2.230 / 2.877 / 2.975 |
| 4, 32 AMM pools, one worker, 240, 30 s | 236.33 / 236.13 / 219.90 | 7,090 / 110 | 7,090 | 2.091 / 2.695 / 2.833 |
| 16, one AMM pool, 240, 30 s | 144.70 / 142.63 / 5.83 | 4,341 / 2,859 | 2,794 | 21.781 / 33.167 / 36.524 |
| 16, one AMM pool, 80, 30 s | 80.00 / 80.00 / 76.80 | 2,400 / 0 | 2,400 | 1.234 / 1.733 / 1.894 |
| 4, eight-Head atomic, 240, 25 s | 227.36 / 227.12 / 142.48 | 5,684 / 316 | 5,342 | 2.903 / 24.587 / 29.711 |
| 4, eight-Head atomic, 120, 20 s | 120.00 / 119.85 / 113.15 | 2,400 / 0 | 2,400 | 1.067 / 1.322 / 1.362 |
| 4, mixed counters, 240, 25 s | 240.00 / 239.40 / 227.76 | 6,000 / 0 | 6,000 | 1.008 / 1.242 / 1.303 |
| 4, independent counters, 240, 25 s | 240.00 / 240.00 / 231.44 | 6,000 / 0 | 6,000 | 0.893 / 1.104 / 1.140 |

The 30-second runs observed an additional 12 seconds of drain.
The 25-second counter runs observed ten seconds and the 20-second
atomic run observed eight. Latency samples
include only transactions seen at common finality, so the upper
percentiles in the overloaded runs omit still-pending transactions.
In the 16-replica pressure run, 38 RPC submissions timed out and 25
of those transactions were subsequently observed. No reported P2P
transport drops or invalid messages occurred. The 16 processes
shared one host and the run observed only 11 common finalized
blocks during the sample. These results give a local overload
point. They leave the effects of replica count, CPU contention and
full-mesh pressure unresolved. The 80 TPS control finalized all
2,400 offers with zero RPC observation errors.

| Run | Aggregate CPU seconds | Peak sampled RSS MB | Linux process writes / RocksDB WAL MB | P2P frame bytes sent / received MB |
| --- | ---: | ---: | ---: | ---: |
| 4, 32 AMM pools | 75.96 | 337.69 | 160.12 / 102.73 | 37.13 / 41.28 |
| 4, one AMM pool | 73.98 | 321.57 | 144.31 / 91.53 | 36.68 / 40.78 |
| 4, 32 AMM pools, one worker | 73.28 | 321.00 | 155.44 / 101.43 | 36.95 / 41.08 |
| 16, one AMM pool at 240 planned TPS | 543.33 | 1,175.40 | 211.16 / 157.59 | 292.38 / 294.90 |
| 16, one AMM pool at 80 planned TPS | 249.85 | 884.55 | 497.61 / 197.63 | 85.52 / 87.00 |
| 4, eight-Head atomic at 240 planned TPS | 88.59 | 320.78 | 115.77 / 88.33 | 39.45 / 43.80 |
| 4, eight-Head atomic at 120 planned TPS | 34.17 | 227.85 | 107.95 / 51.63 | 14.02 / 15.87 |

The four-replica single-pool and distributed AMM samples stayed
within 1% on common visibility throughput. The single pool added
about 229 ms to measured p99. The one-worker comparison gives no
measured throughput improvement from the second worker at this
offered rate. Each is one run. The eight-Head commands recorded
42,736 Head updates and charged 3,960,347 cycles per applied command.
Their repeated 178-command blocks match the fixture's logical read
limit at 46 reads per command. The single-pool and distributed AMM
commands both charged about 3.922 million cycles. The 16-replica
pressure run reached 341 swaps per block during drain, matching
the same budget at 24 reads per swap. The lower atomic run finalized
all 2,400 commands and verified 19,200 Head updates.

The audit replayed all finalized blocks through the observed common
height in a serial Store. It checked Headers, receipts, every Head
version and both authenticated paths. The AMM audit checked both
asset supplies. It found only applied Lineage outcomes among the
measured finalizations. The independent payment control used the
same load driver and finalized all 160 payments.

The archive includes both initial compiler failures, the smoke runs,
and all control and pressure runs. The locked Rust release workspace
passed 460 tests with 16 ignored. The Go proof suite passed.

[Reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Signed workloads, per-transaction timing, replay proofs and failures](benchmarks/2026-11-26-stem-lineage-load.json.xz).

## Proposal selection after the Lineage read limit

Revision `4ffb817` skips candidates whose encoded bytes or minimum
Lineage reads exceed the remaining block budget. Jolteon walks the
mempool by reference. A boundary test spends 18 of 34 reads, skips a
three-target command, and uses the remaining 16 reads for a valid
fee-only outcome. Its Header and receipts match full execution.

The controlled Store probe compared `84049bf` and `4ffb817` with the
same test code and five exact prebuilt RISC-V artifacts. It offered
4,096 signed eight-Head commands in order, with a 1,104-read budget.
Each repetition selected the same 24 commands. Both revisions
produced the same Header and signed workload digest. Median selection
over 11 repetitions fell from **144.588 ms to 5.042 ms**. The timer
excludes constructing the input queue and preparing the selected
block afterward. A baseline checkout can apply fixture commits
`26a9108` and `e0650e1` to run the same probe.

| Local network, planned load | Actual offered TPS, before → after | Common visibility TPS in offer window, before → after | Visible by end of drain, before → after | p99, before → after |
| --- | ---: | ---: | ---: | ---: |
| 4 replicas, eight-Head commands, 240 TPS | 229.32 → 229.64 | 148.88 → 148.88 | 5,680 → 5,680 | 30.523 → 30.271 s |
| 16 replicas, one AMM pool, 240 TPS | 127.60 → 142.63 | 6.20 → 1.33 | 2,108 → 2,116 | 36.099 → 39.182 s |

Both network pairs reused each other's signed workload, genesis,
cadence and resource budgets. The four-replica accounting bottleneck
remained at 178 eight-Head commands per block. The 16-replica
after run admitted more submissions, used 541.35 aggregate CPU
seconds versus 498.40, and remained overloaded. Its sampled
visibility during the offer window was lower. The end-to-end
experiment therefore established no throughput gain. Each row is
one before/after pair on the same host. All observed finalizations
passed the serial Lineage audit.

The archive retains an initial compiler failure, the unpinned
attempt with different contract bytes and Headers, and a pinned
attempt that reused the wrong host test binary in the shared Cargo
target directory. Only the matched Header and checked binaries enter
the Store comparison. The workspace passed 461 release tests with
17 ignored. The Go proof suite and frozen CometBFT build passed.

[Reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Raw timings, signed workloads, pinned contracts and failed controls](benchmarks/2026-11-26-stem-proposal-selection.json.xz).

## Jolteon control transport under Lineage load

Revision `aa33f3f` separates peer transaction relay from consensus
traffic with bounded inbound queues and independent TCP streams.
A transport test filled the relay queue and received a QC frame
on the other socket while the relay socket remained unread.
The scheduler test verified the eight-event control burst and
application progress when both queues had pending events.

All six AMM runs used 16 local replicas on an i9-13900H, matching
signed workloads and genesis at each rate, 250 ms cadence,
two VM workers, and a 2-billion-cycle block cap. Each offered for
30 seconds and observed another 12 seconds. The three traced
240 TPS runs used the same workload driver and independently
pinned node binaries.

| Run | Actual offer / admission / common visible TPS in window | Visible by end of observation | p99 among visible transactions | RPC observation errors | Observed TC views |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline, 240 TPS | 141.90 / 140.33 / 57.03 | 4,257 | 35.942 s | 2 | — |
| Baseline traced, 240 TPS | 125.63 / 123.77 / 22.23 | 2,713 | 35.686 s | 2 | 4 |
| Isolated transport, 240 TPS | 139.13 / 136.97 / 32.97 | 3,035 | 30.828 s | 1 | 6 |
| Isolated transport repeat, 240 TPS | 127.40 / 124.83 / 0.00 | 2,667 | 35.913 s | 5 | 5 |
| Baseline, 80 TPS | 79.50 / 78.93 / 71.37 | 2,385 | 4.948 s | 0 | — |
| Isolated transport, 80 TPS | 80.00 / 79.63 / 72.23 | 2,400 | 3.029 s | 0 | 0 |

A timeout was counted when at least one replica reported its
certificate view in a sampled status. The zero in-window rate means
the observer saw no common finalized transaction during that window.
Status polling also waited on application traffic and sometimes timed
out. The high-load samples still showed accumulation and variable
progress, with no consistent end-to-end throughput improvement.
The traced high-load runs used 534–564 aggregate process CPU
seconds. Outbound drop counters stayed flat during offer and drain.
The 80 TPS after run confirmed all 2,400 offers, with no client drops.
All observed finalizations passed the serial replay and dual-root
Lineage audit.

The new transport recovered a four-replica network after one replica
missed 200 blocks with a 64-block retention window. The 16-replica
run survived two consecutive offline leaders and regained a common
Header after both restarted. A 128-replica payment smoke confirmed
all 96 payments. A separate CKB devnet drove a funded four-member
handoff and delivered a pre-handoff Intercell message at a successor
checkpoint. The locked Rust release workspace passed 463 tests with
17 ignored. The Go proof tests and frozen CometBFT build passed.

[Reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Signed workloads, pacemaker samples, overload and recovery evidence](benchmarks/2026-11-26-stem-transport-priority.json.xz).

## Bounded parallel admission experiment

Revision `d46c8d5` grouped waiting admission requests for the same
finalized tip. With two VM workers it took up to four requests at a
time and stopped extending a group when a query or control message
was waiting. It verified distinct witnessed transactions in parallel
and applied admission decisions in request order. A concurrent RPC
test submitted valid duplicates and an unsigned variant of the same
transaction body. The unsigned variant was rejected and four
replicas confirmed the valid transaction once.

The fixed-tip probe compared 512 signed AMM commands, each involving
two Heads and both asset Type Groups. Two stores used the same
finalized Header. Over 11 alternating repetitions, the median for
512 single-request checks was 54.622 ms. Grouping the same checks in
fours took 28.713 ms. Every receipt matched. This timing covers
Store checks and thread-pool dispatch.

All network samples used 16 local replicas, two VM workers per node,
the same signed workload and genesis at each offered rate, a 250 ms
proposal interval, a 2-billion-cycle block cap, 30 seconds of offer
and another 12 seconds of observation. Baseline refers to `53bb000`.
The traced high-load runs used the same driver with pinned node
binaries. Candidate and final execution used the existing checks.

| Run | Actual offered / admitted / common visible TPS in window | Visible by end of observation | p99 among observed transactions | Replica CPU seconds |
| --- | ---: | ---: | ---: | ---: |
| Baseline, 240 TPS | 142.07 / 140.67 / 67.50 | 4,262 | 35.943 s | 519.72 |
| Baseline repeat, 240 TPS | 130.53 / 128.17 / 5.53 | 2,407 | 37.881 s | 567.80 |
| Batched admission, 240 TPS | 143.87 / 142.00 / 35.40 | 2,767 | 30.985 s | 605.31 |
| Batched admission repeat, 240 TPS | 126.90 / 124.33 / 1.40 | 2,542 | 38.784 s | 629.13 |
| Baseline, 80 TPS | 79.90 / 79.63 / 71.50 | 2,397 | 3.152 s | 308.45 |
| Batched admission, 80 TPS | 80.00 / 80.00 / 75.03 | 2,400 | 2.609 s | 311.69 |

In the first batched high-load run, 4,316 offered transactions
produced 69,056 checks across the 16 replicas. Of 32,710 request
groups, 14,915 contained more than one check. View and RPC
observation timing varied at high load. The fixed-tip speedup
did not translate to a consistent improvement for the full network.
Both high-load batched runs used more aggregate CPU. Their finalized
commands passed the serial replay audit with identical settlement
receipts and conserved AMM supplies.

Revision `a01cc3a` keeps single-request admission, reuses the witnessed
transaction digest for the reply, and exposes per-source counters.
Its 16-replica 80 TPS run confirmed all 2,400 transactions with p99
of 3.352 seconds and zero observation errors. It recorded 2,400
client submissions, 36,000 peer relays and 38,400 Store checks.
That is one full admission check on each replica per offered
transaction in this workload.

The batch experiment passed the two-offline-Leader regression and
a real CKB-funded handoff with an older Intercell Outbox proof.
The production concurrent admission regression passed again.
The locked release workspace passed 464 tests with 18 ignored
on the production path. Go proof tests and the frozen CometBFT
comparison build passed.

[Reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Signed workloads, individual timing samples and default-path validation](benchmarks/2026-11-26-stem-admission-batching.json.xz).

## Jolteon leader-directed transaction relay

Revision `3e0bdb6` compared against `8b48524` with 16 local
replicas and two VM workers per replica. Each rate used byte-identical
signed commands and genesis. Every command traded through the same
AMM with two updated Lineage Heads and both asset Type Groups. The
proposer interval was 250 ms and the block cap was two billion cycles.
Each run offered traffic for 30 seconds and observed for another 12.
The same pinned load driver ran all six samples. The archive includes
the timing of each offered transaction and each observed block.

| Run and planned TPS | Actual offer / admission / all-replica visible TPS in offer window | Visible by end of observation | p99 among observed transactions | Replica CPU seconds |
| --- | ---: | ---: | ---: | ---: |
| Broadcast 80 | 80.00 / 79.93 / 73.87 | 2,400 of 2,400 offered | 3.050 s | 254.74 |
| Directed 80 | 80.00 / 79.90 / 77.43 | 2,400 of 2,400 offered | 1.036 s | 101.19 |
| Broadcast 240 | 142.13 / 139.97 / 11.83 | 2,333 of 4,264 offered | 35.824 s | 540.82 |
| Directed 240 | 146.33 / 146.23 / 112.50 | 4,390 of 4,390 offered | 11.779 s | 313.48 |
| Broadcast 240 repeat | 143.37 / 141.83 / 12.40 | 3,014 of 4,301 offered | 35.944 s | 540.79 |
| Directed 240 repeat | 142.70 / 142.00 / 104.90 | 4,281 of 4,281 offered | 11.588 s | 306.70 |

At 80 TPS the number of received relays dropped from 36,000 to 4,459,
and full Store admission checks from 38,400 to 6,859. The sum of
replica network sends went from 82.8 MB to 65.9 MB. Sampled peak
aggregate resident memory was 906,000–922,000 KiB. Aggregate process
writes were 476.5 MB for broadcast and 596.2 MB for directed relay.
The measured physical writes cover consensus and application data.

At the planned 240 TPS rate, the driver dropped 2,810–2,936 of 7,200
scheduled submissions before sending. The directed runs saw 22,422
and 22,876 relays, versus 63,960 and 64,515 with broadcast. Their
all-replica visibility rates including drain were 123.02 and
124.65 TPS. The baseline logged three and four observation errors.
The directed runs logged zero observation errors and had 509 and
615 transport queue drops across all traffic. For every run the
separate serial Store verified the finalized Headers, receipts, both
Head versions and AMM asset supplies. High-rate p99 covers the
transactions visible within each run's observation window.

The 16-validator fault fixture sent one transaction at view 17, with
both scheduled relay targets stopped. It confirmed on all 14 live
replicas after two local relay retries and one client submission.
The fixture then finished 96 payments, restarted both validators
and verified the two-chain proof and common execution Header.

A four-voter CKB devnet handoff activated the successor at Stem
height 40. The retired replica submitted an Intercell transaction to
the new committee as an observer. All replicas authenticated the
resulting Header and CKB accepted a historical Outbox proof from the
previous epoch. The archive retains two rejected workspace test
attempts caused by the retry test fixture. The corrected fixture uses
the ledger's ordinary Cell input and checks expiry, committee reset and
the end of the retry sweep. The locked release workspace passed 465
tests with 18 ignored. The Go proof suite and frozen CometBFT feature
check passed. A separate 128-validator localnet finalized 128 signed
payments and agreed on the execution Header and two-chain proof.

[Load reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Fault reproduction](OPERATIONS.md#exercise-the-native-jolteon-path) ·
[Signed workloads, raw samples and recovery logs](benchmarks/2026-11-26-stem-jolteon-leader-relay.json.xz).

## Sustained submission and admission latency

Revision `aa0f422` replaces the load driver's fixed sender assignments
with a single paced, bounded queue. It records planned time, worker
start, full frame write, complete reply and positive admission
separately. The first five runs use the same pinned `4907a2c` node
binary, signed workload and genesis. Each runs 16 replicas on one
host with two VM workers per replica, 30 seconds of single-hotspot
AMM commands at a planned 240 TPS and 12 seconds of drain. The
250 ms proposal interval and two-billion-cycle block cap are fixed.

| Driver and workers | Attempted or written TPS | Positive admission TPS | All-replica visible TPS | Visible by end | p99 among visible |
| --- | ---: | ---: | ---: | ---: | ---: |
| Fixed assignments, 24 | 159.80 attempted | 159.63 | 124.70 | 4,794 | 10.118 s |
| Fixed assignments, 64 | 158.77 attempted | 156.93 | 87.17 | 4,763 | 25.630 s |
| Shared queue, 24 | 161.87 written | 161.07 | 123.20 | 4,856 | 9.219 s |
| Shared queue, 64 | 178.80 written | 177.37 | 98.07 | 5,237 | 30.643 s |
| Shared queue, 64 repeat | 173.67 written | 171.27 | 96.17 | 5,063 | 31.511 s |

The three rates use the offer window. The earlier driver counted an
attempt at worker start. The shared queue verified 5,364 and 5,210
complete frame writes in the two 64-worker runs. It attempted no
send after the queue deadline and recorded 1,836 and 1,990
unattempted submissions, respectively. Increasing available senders
raised offered traffic in these runs while p99 increased. All
observed finalized AMM transactions passed the independent serial
Header, receipt, Head version and asset-supply audit.

An instrumented node run with the same AMM fixture wrote 175.17 TPS
and observed 85.47 TPS on every replica during the offer window.
The p95 from worker start to a full frame write was 307 μs. The
p95 from that write to a complete reply was 1.350 s. Of 5,255
decoded submissions, 2,018 waited over 100 ms to enter the replica
handler and 967 waited over 500 ms. All 16 replicas recorded such
long waits. Mean handler queue time was 263.85 ms. Once handling
started, 5,249 replies were encoded within 10 ms and all finished
within 100 ms. Mean handler service time was 2.78 ms. The client
and server measurements identify admission queueing as the main
source of the measured response tail for this workload.

The separate four-replica payment fixture uses 64 workers, 20 seconds
of offer and 12 seconds of drain, the same 250 ms interval and
two-billion-cycle block budget. It signs an independent ordinary
payment for each funded input. At the 1,000 TPS setting the driver
wrote all 20,000 frames in the offer window with zero client or
transport write drops. Positive admission reached 999.55 TPS.
All four replicas saw 940.15 TPS in the offer window and 968.94 TPS
including drain. All 20,000 payments finalized with p99 of 1.190 s.
The 2,000 TPS setting on the same four-replica configuration wrote
27,850 of 40,000 planned frames and finalized every written payment.
All-replica visibility was 1,280.55 TPS during offer and 1,338.10 TPS
including drain. The 12,150 losses were client queue drops or
expired jobs before attempting the write.

| Planned payments/s | Written TPS | Positive admission TPS | p50 / p95 / p99 | Aggregate CPU s | Process writes / network sends | Sampled peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 1,000.00 | 999.55 | 0.911 / 1.134 / 1.190 s | 41.55 | 342.3 / 34.6 MB | 540 MiB |
| 2,000 | 1,392.50 | 1,389.30 | 1.404 / 1.878 / 1.945 s | 57.31 | 418.5 / 51.9 MB | 734 MiB |

Replica resource counters cover offer and drain. Latency starts at
the complete client frame write and ends at sampled common
visibility. Both payment runs verified the common Header and the
two-chain finality certificate. The locked release workspace passed
465 tests with 18 ignored. The Go proof suite and frozen CometBFT
feature check passed.

The first payment-test invocation used an unmatched filter and ran
zero tests. The archive retains that invocation alongside the
corrected run and the signed workloads.

[Reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Raw timing, signed workloads, proof and failed invocation](benchmarks/2026-11-26-stem-load-dispatch.json.xz).

## Reuse of staged proposal execution

Revision `eff6aad` measures Leader construction, uncached candidate
execution and certified commit. Revision `33721dd` retains the staged
overlay and receipts for eligible proposals. It binds the final
transaction root to the new Live Cell origins before constructing
the authenticated batch. The fixed test compares both paths against
serial verification with the same parent, timestamp and transactions.
It selects 24 signed Lineage commands from 4,096 under the same
1,104-read budget. With two VM workers and 11 alternating trials,
the median fell from 10.436 ms to 6.592 ms. Transaction selection,
the complete Header and receipts agreed in every trial.

All measurements below use one host, two VM workers per replica,
a 250 ms proposal interval and a two-billion-cycle block cap. The
signed workload and genesis are identical within each comparison.
The high hotspot runs use 16 replicas, 64 submitters and 30 seconds
of traffic. Payments use four replicas, 64 submitters and 20 seconds.
Each run observes another 12 seconds after the offer window.

| Workload and Leader path | Written / admitted / common visible TPS in offer window | Visible by end | p99 among visible | Mean proposal construction |
| --- | ---: | ---: | ---: | ---: |
| Hotspot, planned 240, replay | 180.47 / 176.90 / 98.47 | 5,196 | 32.408 s | 479.21 ms |
| Hotspot, planned 240, reuse | 158.93 / 155.13 / 81.33 | 4,768 | 26.191 s | 178.33 ms |
| Hotspot, planned 240, reuse repeat | 164.03 / 161.43 / 104.83 | 4,921 | 21.999 s | 149.11 ms |
| Payments, planned 2,000, replay | 1,323.50 / 1,320.30 / 1,217.10 | 26,470 | 2.104 s | 99.57 ms |
| Payments, planned 2,000, reuse | 1,350.50 / 1,349.20 / 1,223.75 | 27,010 | 2.249 s | 95.87 ms |
| Hotspot, matched 120, replay | 120.00 / 119.73 / 110.97 | 3,600 | 2.723 s | 76.24 ms |
| Hotspot, matched 120, reuse | 120.00 / 120.00 / 114.13 | 3,600 | 2.263 s | 43.68 ms |

The high hotspot runs reached different written rates and proposed
25, 37 and 44 blocks, respectively. Their means include empty blocks.
They show shorter construction and lower observed p99 in the reuse
runs with mixed common-visibility throughput. In the replay run,
uncached candidate execution averaged 814.54 ms over 375 observations
and certified commit averaged 6.80 ms over 400 observations. In the
payment pair both versions finalized every written transaction.
Measured process writes for the four replicas totaled 406.87 MB and
415.38 MB, about 3.84 kB per finalized payment per replica in each
run. These are local process-write counters.
Cell Patricia nodes accounted for 77.33% and 77.30% of the serialized
finality batches in the payment pair.

The matched 120 TPS pair uses 24 submitters and all 3,600 writes
in both runs. Aggregate replica CPU time was 185.38 and 206.01
seconds. Every observed finalized AMM command in the controls and
reuse runs passed the separate serial replay audit. A same-block
payment test verifies the final origin against the Header and
reopens its authenticated proof. The Keeper test checks that
reserved and expired work produces identical Headers and receipts
through proposal building and full preparation.

The 16-replica fault run confirmed 96 payments with two Leaders
stopped and verified the common Header after restart. Its first
attempt hit an occupied port. Both attempts are archived. A real
CKB devnet run completed a funded four-voter handoff at Stem height
40 and delivered a historical Intercell Outbox proof on CKB.
The locked release workspace passed 465 tests with 18 ignored.
The Go proof suite and frozen CometBFT comparison build passed.

[Reproduction](OPERATIONS.md#measure-sustained-lineage-commands) ·
[Raw samples, signed workloads and regression logs](benchmarks/2026-11-26-stem-proposal-reuse.json.xz).

## Compact Patricia branch storage

RocksDB stores only the significant prefix bytes of each compressed
Patricia branch. The editor and proof verifier receive canonical node
bytes. The fixed RocksDB fixture seeds 8,192 leaves and then replaces
512 leaves in a single edit. It measures 11 alternating warm trials
per layout, reconstructs the same root and compares all canonical
node mutations.

| Fixed editor sample, median | Full prefix | Compact prefix |
| --- | ---: | ---: |
| Node reads | 4,120 | 4,120 |
| Node value bytes read | 376,370 | 277,207 |
| Serialized node WriteBatch bytes | 599,346 | 503,521 |
| RocksDB fetch | 1.859 ms | 1.829 ms |
| Copy or restore canonical bytes | 0.058 ms | 0.100 ms |
| Entire tree edit | 5.082 ms | 5.198 ms |
| Rehash of newly written canonical nodes | 0.861 ms | 0.868 ms |
| WriteBatch builder | 0.317 ms | 0.318 ms |

The rehash probe measures a separate pass over the newly produced
nodes. The total edit time also includes validation of loaded nodes.

The payment comparisons use a pinned load driver and four replicas
on one host, 64 submission workers, two VM workers per replica, a
250 ms proposal interval and a two-billion-cycle block budget. Each
pair uses byte-identical signed workloads and genesis with fresh
replica databases and a 12-second drain allowance. The baseline node
is `22fc5a6` and the compact node is `79cb103`.

| Offer window, planned TPS, layout | Written / admitted / common visible TPS in offer window | Visible by end | p99 | Cell trie batch bytes | WAL bytes | Process write bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 20 s, 1,000, full | 1,000.00 / 998.10 / 939.30 | 20,000 | 1.221 s | 133.67 MB | 193.25 MB | 345.39 MB |
| 20 s, 1,000, compact | 1,000.00 / 1,000.00 / 957.70 | 20,000 | 1.313 s | 110.63 MB | 170.17 MB | 244.91 MB |
| 20 s, 2,000, full | 1,339.30 / 1,336.10 / 1,230.25 | 26,786 | 2.028 s | 178.48 MB | 257.60 MB | 408.28 MB |
| 20 s, 2,000, compact | 1,326.15 / 1,322.95 / 1,222.75 | 26,523 | 2.054 s | 148.65 MB | 227.01 MB | 382.26 MB |
| 60 s, 1,000, full | 808.58 / 807.52 / 780.10 | 48,515 | 7.448 s | 414.12 MB | 557.52 MB | 969.23 MB |
| 60 s, 1,000, compact | 809.15 / 808.80 / 786.05 | 48,549 | 3.707 s | 334.17 MB | 477.54 MB | 787.42 MB |

The 20-second 1,000 TPS pair reduced serialized Cell trie writes by
17.23% and WAL bytes by 11.94%. Aggregate replica CPU was 44.65 and
45.63 seconds. In the 60-second pair, the generator dropped 11,485
and 11,451 offers before writing, every written payment confirmed,
and aggregate replica CPU was 155.00 and 193.25 seconds. The two
long runs provide one matched sample of write volume and lag.

The bundled RocksDB build disables IOStatsContext. Its flush and
compaction byte tickers stayed at zero in these runs. The table below
sums actual SST file sizes from retained RocksDB creation events,
including rotated logs. This covers database startup through the end
of each run. The full-prefix 20-second 1,000 TPS fixture also reopened
its archive and generated a separate 1.96 MB recovery SST. Linux
process-write counts cover all writes charged to replica processes.

| Offer window and planned TPS | Full-prefix flush SSTs | Compact flush SSTs |
| --- | ---: | ---: |
| 20 s, 1,000 | 4 files, 76.88 MB | 0 files |
| 20 s, 2,000 | 4 files, 92.36 MB | 4 files, 94.92 MB |
| 60 s, 1,000 | 12 files, 306.68 MB | 8 files, 210.37 MB |

No compaction SST creation appeared in these runs. Longer runs are
needed to measure sustained compaction and device-level writes.

The branch codec test round-trips all 256 prefix lengths and rejects
malformed lengths. A mixed four-replica network used two executables
with each layout, confirmed 96 payments and restarted a compact
replica. All four compared the same execution Header, and the new
executable verified the finality proof served by the full-prefix
archive. Opening an old database returned the expected layout
error. A separate CKB devnet run completed a funded handoff at
Stem height 40 and delivered a historical Intercell proof. The
locked release workspace passed 466 tests with 19 ignored. The Go
proof tests and frozen CometBFT feature check passed.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw writes, timing, signed workloads and regressions](benchmarks/2026-11-26-stem-patricia-storage.json.xz).

## Bounded submission connection reuse

This round tested whether opening a TCP connection for each signed
payment limits sustained admission. The node accepts sequential
`SUBMIT` frames on a socket for up to 256 requests. It closes a
submission socket after a one-second idle read deadline. The Rust
SDK exposes reuse explicitly with up to four idle sockets per
client. Framing, validation, queue ordering and per-request admission
responses are shared with the fresh-connection path.

Four replicas used the same pinned fixture, a 250 ms proposal interval,
a two-billion-cycle block cap and two VM workers per replica. Each
comparison used byte-identical signed payments and genesis and a
12-second drain allowance. The 700 TPS pair used 24 submitters and
each other run used 64. The `e5b806f` node establishes the preceding
single-request baseline. The fresh and reuse samples use the same new
node binary. The 60-second pair ran in reuse-then-fresh order.

| Planned TPS, duration, sockets | Written / admitted / common visible TPS in offer window | Confirmed by end | Client drops | Reused requests | p99 finality visibility |
| --- | ---: | ---: | ---: | ---: | ---: |
| 700, 20 s, fresh | 700.00 / 699.65 / 671.25 | 14,000 | 0 | 0 | 0.910 s |
| 700, 20 s, reuse | 700.00 / 699.60 / 671.80 | 14,000 | 0 | 12,915 | 0.910 s |
| 2,000, 20 s, prior node | 1,414.10 / 1,414.10 / 1,329.30 | 28,282 | 11,718 | 0 | 1.945 s |
| 2,000, 20 s, fresh | 1,379.55 / 1,376.35 / 1,270.70 | 27,591 | 12,409 | 0 | 1.895 s |
| 2,000, 20 s, reuse | 1,383.00 / 1,379.80 / 1,275.50 | 27,660 | 12,340 | 23,061 | 1.979 s |
| 1,000, 60 s, fresh | 997.62 / 997.62 / 979.07 | 59,857 | 143 | 0 | 1.579 s |
| 1,000, 60 s, reuse | 883.23 / 882.17 / 856.57 | 52,994 | 7,006 | 41,716 | 2.612 s |

Every completed write in these comparisons became visible on all four
replicas. Client drops are scheduled submissions lost before the start
of a write. At 700 TPS, reuse saved 92.25% of connection establishments.
Median socket readiness was 54 μs with fresh sockets and 3 μs with
reuse. At the 2,000 TPS setting median readiness was 33 μs and 1 μs.
The p95 response time from frame write was 190.31 ms and 195.48 ms.
Mean node admission queue time was 29.15 ms and 29.46 ms. Mean handler
service time was 0.30 ms and 0.32 ms. The run with reuse spent 59.44 aggregate replica CPU seconds
against 58.86 for fresh connections.

The long pair reached different actual offered rates. The fresh
run consumed 151.26 aggregate replica CPU seconds, compared with
182.01 for reuse. Uncached candidate verification averaged 88.92 ms
and 144.40 ms, respectively. This single pair shows degradation in
the reuse run with several node timings moving together. Reuse
remains an explicit SDK option. The matched short samples show
substantially fewer connections and essentially flat replicated
throughput.

A separate 30-second profile used the 2,000 TPS setting and enabled
reuse. It fully wrote and confirmed 39,663 signed payments, with
1,251.63 common-visible TPS during offer. `perf record` sampled the
four replica processes for 12 seconds at a requested 199 Hz per
active thread. The `cpu-clock:u` recording has 6,432 userspace
samples and zero lost samples. Flat samples include
`Decoder::decode_raw` (8.60%), `Decoder::decode` (5.18%),
`rvc::factory` (5.01%), and `allocate_core` (8.26%).
`memset` accounted for 11.32%. Flat samples leave its callers
unattributed. The reusable assembly VM resets all 8,192 trace slots
and lazily initializes guest frames. The sample identifies areas to
isolate in a separate controlled VM experiment.

The transport test exchanges all 256 request/reply pairs and checks
connection closure. A four-replica test rejects an unsigned
transaction, accepts the signed transaction on the same socket, and
waits for all replicas to see it. The locked release workspace passed
467 tests with 19 ignored. The Go proof suite and frozen CometBFT
feature check passed. Sixteen replicas finalized 96 payments with two
Leaders offline and agreed on the execution Header after restart.
A real CKB devnet funded a four-voter handoff at height 40 and
accepted a historical Intercell Outbox proof at a later checkpoint.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Signed workloads, raw latency, CPU samples and regression logs](benchmarks/2026-11-26-stem-submission-reuse.json.xz).

## Standalone CKB Metatype composition

CKB 0.210.0 deployed the dispatcher, its immutable adapter table,
and two separately compiled RISC-V adapters. Each mint creates
two Metatype Cells. A one-asset fixture uses one shared asset group
and up to 15 independent transfer policy groups with different
arguments. Two manifests of different sizes share groups except at
the two-constraint setting. The spend splits the larger Cell and
preserves the total for each old policy. The 85-byte witness is a
standard CKB Lock witness.

| Distinct groups | Largest manifest and state | Largest occupied Capacity | Mint bytes | Mint cycles | Split bytes | Split cycles |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 2 | 323 B | 481 CKB | 1,597 | 2,244,889 | 2,198 | 2,332,437 |
| 4 | 693 B | 851 CKB | 1,967 | 2,858,167 | 2,938 | 3,137,120 |
| 8 | 1,433 B | 1,591 CKB | 2,707 | 4,224,550 | 4,418 | 5,037,981 |
| 16 | 2,913 B | 3,071 CKB | 4,187 | 7,246,903 | 7,378 | 9,683,231 |

Occupied Capacity includes the actual serialized script args and
the live Cell Data under CKB's occupied-byte rules. The fixture
funds additional spendable Capacity to cover splits and rejected
large-state cases. Split transactions took 415–427 ms from
submission to inclusion under scripted local mining. This is a
single-machine devnet observation.

One atomic transaction spends two distinct UDT groups and their
policies, and sends part of both assets into one Metatype Cell.
It consumed 3,880,281 cycles, occupied 3,468 transaction bytes, and
used an 85-byte witness. The fixture verifies the CKB transaction
and witness inclusion proof for every accepted transition.
The CKB dispatcher rejected deletion or substitution of an input
policy, a duplicate constraint, missing adapter or table, transfer
into another deployment, an unapproved recipient, excess supply,
unknown logical code and excess entry state. Each rejection checks
the dispatcher error code. The first attempt lacked Capacity to reach
the state limit. Its invocation and signed transactions are retained
with the corrected run.

The separate four-replica regression re-exercised the specialized
two-Type object bridge on CKB and Stem. CKB mint, transfer and burn
used 4,260,992, 3,302,200 and 3,409,457 cycles and 2,183, 886 and
1,523 bytes. It includes a Jolteon checkpoint and an independent
approval Lock. These are different operations from the standalone
Metatype UDT fixture. A same-workload cost comparison awaits the
Metatype object and bridge path. The first control used an obsolete
object ELF with an earlier Intercell witness format. The archive
retains its rejection and the successful run with rebuilt binaries.
Separate four-replica controls verified original Spore ID settlement
and the native UDT round trip. The locked Rust release workspace
passed 470 tests with 19 ignored. Go proof tests and the frozen
CometBFT feature check passed.

[Reproduction](OPERATIONS.md#exercise-standalone-ckb-metatype) ·
[Raw CKB evidence and controls](benchmarks/2026-11-26-stem-metatype-ckb.json.xz).

## Metatype object approvals and Native Polytype comparison

CKB 0.210.0 executed the dispatcher with a genesis-pinned deployment
and two separately deployed RISC-V adapters. The identity adapter
verified one live head and immutable content. The approval adapter
verified independent Ed25519 seals from the group witness envelope.
Two distinct authorities approved two objects in the same transaction.
All CKB samples use the stock devnet verification settings.

| CKB operation | Estimated cycles | Transaction bytes | Witness bytes | Metatype envelope |
| --- | ---: | ---: | ---: | ---: |
| Issue two objects | 7,330,304 | 2,145 | 508 | 423 B |
| Transfer two objects atomically | 7,328,877 | 2,249 | 508 | 423 B |
| Refresh both, with new approvals | 7,375,784 | 2,249 | 508 | 423 B |
| Transfer one object | 4,537,863 | 1,413 | 308 | 223 B |

The two objects occupy 531 and 516 CKB. The local submissions
confirmed in 316–424 ms using scripted mining. The fixture checked
transaction and witness inclusion, rejected 14 negative cases, and
verified that the authority's valid seal could not override
immutability. It also rejected missing or foreign genesis, duplicate
or noncanonical envelope rows, unknown group, crossed approvals,
wrong recipient or content proof, removals, duplicate objects and
seals from old OutPoints.

Four native Jolteon replicas exercised eight normalized object
cases with the same underlying content and authority roles.
Issuance, atomic transfer, and refresh passed in both protocols.
Forged identity, missing approval, edited content with approval,
removed policy, and duplicate identity failed in both. The native
authority uses a separately signed Cell input. The Metatype
authority uses a detached seal. The native fixture compared all
four Headers and live output bytes. The released WASM verifier
independently authenticated three finality and execution proofs
and rejected an altered receipt for each.

| Two-object operation | Native encoded bytes | Native receipt cycles |
| --- | ---: | ---: |
| Issue | 1,487 | 5,875,076 |
| Transfer atomically | 1,577 | 5,866,145 |
| Refresh | 1,577 | 5,866,145 |

The specialized CKB object transfer control used the same 66-byte
content and an independent approval. It used 3,317,259 cycles and
886 bytes. Standalone Metatype used 4,537,863 cycles and 1,413 bytes
for that operation. The specialized Type enforces a fixed object
mapping and consumes a signed approval Cell. Metatype carries an
inline Manifest, a group witness and a genesis Header Dep. The
comparison covers local transfer. A matched Metatype bridge mint
and burn remains open.

The new genesis-anchored dispatcher also passed the standalone UDT
matrix. The 16-group split used 9,867,333 cycles and 7,506 bytes,
against 9,683,231 cycles and 7,378 bytes in the earlier dispatcher.
The largest Cell now occupies 3,103 CKB, including the genesis hash
in the physical Type args. Both raw rounds remain available.
The first native comparison provided a JSON object to a CLI that
expects a hex proof. The second assumed a single rejection code for
duplicate identity, which both the native identity and policy can
reject. The corrected fixture verified all matched cases.

The locked Rust release workspace passed 472 tests with 19 ignored.
Go proof tests and the frozen CometBFT feature check passed.
Separate real CKB and four-replica Stem controls passed specialized
compound object, Spore, native UDT and funded Jolteon handoff with
historical Intercell delivery.

[Reproduction](OPERATIONS.md#exercise-immutable-objects-and-independent-approvals) ·
[Signed cases, failures, proofs and exact commands](benchmarks/2026-11-26-stem-metatype-object.json.xz).

## Immutable Metatype custody round trip

Four native Jolteon replicas and CKB 0.210.0 finalized an original
Polytype object, locked it under the generic escrow, minted its full
Manifest as a CKB Metatype Cell, transferred it with the independent
application approval, burned the representation, and restored the
native Cell. The complete run reached common Stem height 45.
A separate WASM verifier authenticated the native issue, export and
return receipts from their finalized Headers. It checked membership
of the original escrow at height 28, non-membership of that same
OutPoint at height 45, and membership of the returned Cell at height
45. The second run reproduced the round trip and the rejection set.

The matched fixture switches CKB profiles while keeping the original
66-byte object, two native Type Scripts, independent approval Lock
and transaction roles. The specialized and Metatype runs below
used the same Stem and CKB binaries. The CKB node estimated actual
script cycles and verified each committed transaction and witness
inclusion proof. Proof sizes can vary among independently initialized
devnets.

| CKB operation | Specialized cycles | Metatype cycles | Specialized bytes | Metatype bytes | Specialized / Metatype witness bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Import from Stem | 4,256,272 | 4,954,725 | 2,183 | 2,600 | 896 / 794 |
| Transfer | 3,306,776 | 4,046,888 | 886 | 1,373 | 170 / 170 |
| Burn and return | 3,447,165 | 4,122,449 | 1,523 | 1,735 | 170 / 170 |

Occupied Capacity was 256 CKB in the specialized fixture and 563 CKB
in the Metatype fixture. The increase covers the inline original
Scripts and projection data. The native receipt cycles for issue,
export and return were 3,897,526, 4,041,264 and 14,113,742. On CKB the
separate Metatype adapters require the same paired authority Lock.
This bridge run uses ordinary CKB input signatures for that authority.
The standalone detached-seal profile is measured separately above.

The Metatype fixture rejected 22 cases, including wrong Manifest at
export, missing source policy at import and transfer, a foreign
Metatype deployment, import without the pinned mailbox, missing
Jolteon checkpoint, unapproved import, transfer and return, returning
without burning, replay on CKB, and repeat release on Stem.
The first three attempts are preserved. The first failed to encode
a mailbox hash in the fixture. The second had insufficient CKB
funds for the deliberate remint attempt. The third reached the
native return and rejected the missing policy at the escrow Lock,
while the fixture expected its independent Type Script error.

The first locked Rust release workspace run reached its 260-second
timeout. The retry passed 474 tests with 19 ignored. The Go proof
tests and frozen CometBFT feature check passed. A rebuilt Stem node
repeated the round trip and the independent WASM verification.
Standalone Metatype UDT and object, Spore, native UDT, and a funded
four-to-four Jolteon handoff with historical Intercell delivery
passed on the devnets.

[Reproduction](OPERATIONS.md#exercise-stem-to-ckb-metatype-custody) ·
[Signed runs, rejected controls and proof verification](benchmarks/2026-11-26-stem-metatype-custody.json.xz).

## Sparse assembly trace invalidation

The earlier payment profile attributed 8.26% of userspace samples to
the allocation pool checkout path. Its reset wrote the length field in
all 8,192 trace slots. Each assembly core now uses a 1 KiB bitmap and
clears the length of each populated slot on reset or pool checkout.
The trace lookup still tests the original address and nonzero length.
Guest cycle accounting continues to use the installed trace metadata.

The isolated release probe measured trace publication plus invalidation
on a reused core. The probe permutes slot indices and reports the median
of 14 samples per revision and size. The synthetic probe writes trace
payloads directly.

| Populated trace slots | Full sweep, μs | Bitmap, μs |
| --- | ---: | ---: |
| 4 | 8.776 | 0.126 |
| 16 | 8.187 | 0.224 |
| 64 | 9.092 | 0.418 |
| 256 | 11.567 | 2.733 |
| 1,024 | 22.077 | 11.358 |
| 8,192 | 130.946 | 159.012 |

The full-table stress case includes extra bitmap updates for every
publication and pays a higher total cost. Trace counts in the probe are
controlled inputs, and the table does not describe their distribution
in the payment workload.

Four local replicas used the same pinned test executable, signed payment
bytes and genesis for each rate. All runs used fresh connections, 64
senders, two VM workers per node, a 250 ms proposal interval and a
two-billion-cycle block cap. The matched 1,000 TPS offer lasted 35
seconds. Each 2,000 TPS offer lasted 24 seconds. The high-load pair was
repeated in reverse order.

| Planned TPS, sample | Written / admitted / common visible TPS in offer | Finalized TPS with drain | Confirmed / client drops | Common visibility p50 / p95 / p99 | Aggregate replica CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1,000, full sweep | 998.54 / 997.14 / 959.57 | 976.10 | 34,949 / 51 | 1.090 / 1.397 / 1.497 s | 85.43 s |
| 1,000, bitmap | 999.17 / 997.34 / 961.00 | 978.64 | 34,971 / 29 | 1.006 / 1.259 / 1.326 s | 82.32 s |
| 2,000, full sweep | 1,294.67 / 1,292.00 / 1,206.38 | 1,244.51 | 31,072 / 16,928 | 1.514 / 2.038 / 2.135 s | 74.09 s |
| 2,000, bitmap | 1,175.04 / 1,174.00 / 1,111.25 | 1,139.67 | 28,201 / 19,799 | 1.514 / 2.709 / 3.602 s | 71.60 s |
| 2,000, bitmap repeat | 1,371.96 / 1,369.29 / 1,282.92 | 1,317.12 | 32,927 / 15,073 | 1.434 / 1.931 / 2.024 s | 72.35 s |
| 2,000, full sweep repeat | 1,283.50 / 1,280.83 / 1,195.83 | 1,237.14 | 30,804 / 17,196 | 1.525 / 2.067 / 2.157 s | 74.99 s |

All written payments became visible on all four replicas. The matched
1,000 TPS run showed nearly the same confirmation throughput and used
3.6% less replica CPU in the bitmap run. Measured process writes were
505.90 and 527.52 MB, and the replicas sent 57.42 and 55.67 MB over
peer connections, respectively. The two higher-load pairs moved in
opposite directions and wrote different numbers of payments. The
sustained throughput result remains inconclusive.

The separate instrumented pair sampled all four processes at a requested
199 Hz per active thread for 12 seconds. The full-sweep run collected
6,449 userspace samples, of which 606 landed in the checkout symbol
(9.40%). The bitmap run collected 6,303 samples, of which 16 landed in
the new explicit trace invalidation function (0.25%). Both recordings
reported zero lost samples. The functions have different inlining
boundaries. The totals show the change in the visible hotspot, while
the isolated probe measures the cache operation directly. Raw
instruction decoding still accounted for 9.47% of samples in the
bitmap run.

The final locked release workspace passed 475 tests with 19 ignored.
The regression test exercises reloading different instructions at two
cached addresses, both after an in-place reset and after allocation
reuse, and compares the result and cycles with a fresh core. Both
vendored AUIPC tests passed. The generated assembly offsets matched the
header and the AArch64 backend assembled successfully. The Go proof
tests and frozen CometBFT feature check passed. The rebuilt node
completed an immutable Metatype round trip on real CKB and a funded
four-voter Jolteon handoff with historical Intercell delivery.

The focused package build was interrupted when its separate feature
graph started rebuilding RocksDB. The first workspace build exposed
an ambiguous pointer type in the regression test. Both logs are
included alongside the successful retry.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[All runs, signed workloads, CPU samples and regression logs](benchmarks/2026-11-26-stem-vm-trace-reset.json.xz).

## RISC-V instruction-width dispatch

The standard decoder now routes each cache miss to the compressed
decoder or the configured 32-bit ISA factories using the instruction's
low two bits. It preserves the precedence of built-in 32-bit
factories and the insertion order of custom factories. The executable
memory, instruction fetch, macro-op fusion and cycle accounting code
is shared with the preceding revision.

The isolated probe loaded each real RISC-V ELF in the assembly core
using the VM's normal loader. It reset the instruction cache before
each scan of the executable segments. The figures are the median of
18 samples per revision and corpus. The corpus counts and decoded
checksums matched across revisions. This probe measures cold
decode calls and uses sequential instruction addresses from the ELF.

| Executable corpus | Total / compressed instructions | Prior ns/instruction | Width dispatch ns/instruction | Reduction |
| --- | ---: | ---: | ---: | ---: |
| Stem standard Lock | 6,668 / 3,568 | 11.869 | 10.663 | 10.2% |
| CKB Metatype dispatcher | 17,299 / 9,335 | 14.299 | 12.591 | 11.9% |
| Stem native object Type | 10,838 / 5,719 | 13.348 | 11.759 | 11.9% |

The decoder compared 3,932,160 results across RV32 and RV64, three VM
versions and four extension selections. It checked all 49,152 narrow
encodings, including reserved patterns, and a stratified 32-bit
sample by primary opcode, function selectors and register fields.
The focused tests also checked custom factory order, fallback from
built-ins and the executable-page boundary for both instruction
widths.

The paired network runs used four local Jolteon replicas, fixed signed
workloads and genesis, fresh sockets, 64 senders, two VM workers per
replica and a 250 ms proposal interval. The block limit was two billion
cycles. The 1,000 TPS offer lasted 35 seconds and the 2,000 TPS offers
lasted 24 seconds with a 12-second drain allowance. The second higher
load pair reversed the revision order.

| Planned TPS and revision | Written / admitted / common visible TPS in offer | Finalized TPS with drain | Confirmed / client drops | Visibility p50 / p95 / p99 | Replica CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1,000 prior | 1,000.00 / 999.37 / 969.74 | 979.67 | 35,000 / 0 | 0.932 / 1.187 / 1.256 s | 74.26 s |
| 1,000 width dispatch | 1,000.00 / 999.46 / 971.23 | 979.43 | 35,000 / 0 | 0.947 / 1.188 / 1.252 s | 74.02 s |
| 2,000 prior | 1,389.58 / 1,388.71 / 1,323.67 | 1,350.12 | 33,350 / 14,650 | 1.408 / 1.874 / 1.934 s | 70.01 s |
| 2,000 width dispatch | 1,432.75 / 1,432.25 / 1,328.79 | 1,391.00 | 34,386 / 13,614 | 1.373 / 1.838 / 1.902 s | 68.25 s |
| 2,000 width dispatch repeat | 1,445.88 / 1,445.83 / 1,372.88 | 1,404.16 | 34,701 / 13,299 | 1.349 / 1.768 / 1.814 s | 68.51 s |
| 2,000 prior repeat | 1,421.21 / 1,418.54 / 1,332.46 | 1,370.63 | 34,109 / 13,891 | 1.387 / 1.857 / 1.924 s | 69.86 s |

All written payments became visible on all four replicas. At the
matched 1,000 TPS offer, both revisions finalized all 35,000 payments
and used almost the same CPU time. The two overload pairs reached
different actual write rates. Width dispatch coincided with 0.4% and
3.0% higher common-visible throughput and 2.5% and 1.9% lower
aggregate replica CPU across the two pairs. In the matched 1,000 TPS
run, aggregate process writes were 525.06 and 524.20 MB. Replica peer
traffic was 55.65 and 57.55 MB. The offer rate reached by the clients
in the overloaded runs was approximately 1,390–1,446 TPS.

The isolated CPU profiles sampled all four replicas at a requested
199 Hz per active thread for 12 seconds. The prior and width-dispatch
runs yielded 6,275 and 6,617 userspace samples, with zero losses.
Flat samples in raw decoding moved from 9.05% to 8.45%. The
compressed factory moved from 5.00% to 3.05%. The new standard
dispatch function accounted for 1.66% of samples. These samples show
where the CPU spent time in the profiled runs. The ELF probe measures
isolated decoder cost.

The locked release workspace passed 475 tests with 19 ignored. The
separate locked vendored CKB-VM assembly suite passed all 167 tests,
including the differential tests, VM versions, instruction extensions,
reset and resume cases. The frozen CometBFT build and Go proof tests
passed. Four Stem replicas and real CKB completed the immutable
Metatype round trip and a funded four-member Jolteon handoff with
historical Intercell delivery. The first Metatype invocation stopped
at setup because its output directory matched an ELF corpus filename.
The corrected run and the first failure are both included.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Exact ELF bytes, signed workloads, raw profiles and regression logs](benchmarks/2026-11-26-stem-vm-decoder.json.xz).

## Shared Metatype manifest definitions

The same RISC-V dispatcher and adapter ELFs ran both layouts on
CKB 0.210.0. Each row uses two UDT inputs and three outputs with
the same logical amounts and Script sets. The long set occupies
two output Cells. The short set occupies the third. The two-group
row uses one definition for all three outputs. Comparison Capacity
includes one live definition per distinct set used by those outputs.
The fixture's unrelated Cells and definitions published only for
rejection cases are outside those totals.

| Groups in long set | Inline outputs, CKB | Shared outputs plus definitions, CKB | Inline / shared bytes | Inline / shared cycles |
| --- | ---: | ---: | ---: | ---: |
| 2 | 1,539 | 729 + 368 = 1,097 | 2,326 / 1,553 | 2,387,056 / 2,448,788 |
| 4 | 2,279 | 737 + 1,106 = 1,843 | 3,066 / 1,598 | 3,237,639 / 3,407,592 |
| 8 | 3,759 | 753 + 1,846 = 2,599 | 4,546 / 1,614 | 5,136,747 / 5,840,190 |
| 16 | 6,719 | 785 + 3,326 = 4,111 | 7,506 / 1,646 | 9,946,058 / 12,372,414 |

The 16-group case saves 38.8% of the occupied Capacity after
counting definitions and 78.1% of the transaction bytes. Cycles
increase 24.4%. Each split carries 85 witness bytes in both layouts.
Local submission through mining took 416–426 ms across those eight
measurements. The archive records issuance, definition publication,
turnover and all individual inclusion proofs and timings.

The mixed-layout transaction combines two different assets in the
same output and keeps one asset output inline. Two short and long
manifests contribute to shared virtual groups. CKB accepted the
mixed-layout transaction at 4,325,580 cycles and 2,769 bytes. The
all-inline control used 3,989,029 cycles and 3,596 bytes. The fixture
spent an original definition and used its byte-identical reissue
at a new OutPoint. CKB rejected 19 cases in the shared run. These
include an unavailable definition, malformed and nested definitions,
state count mismatches, changed policy obligations and excessive
expanded size. The latter used 19 outputs with 79,059 compact bytes
and 132,867 expanded bytes.

Four Stem replicas and CKB completed matched immutable object
custody round trips. Both paths used the same object content,
source Type Scripts and independent approval. The shared import
rejects an omitted definition. The dedicated definition requires
368 CKB, so total occupied Capacity for this one represented
object is 661 CKB with a shared definition and 563 CKB inline.
The represented Cell itself occupies 293 CKB in shared form.

| CKB custody step | Inline / shared bytes | Inline / shared cycles |
| --- | ---: | ---: |
| Import | 2,702 / 2,367 | 5,014,174 / 5,027,964 |
| Transfer | 1,373 / 1,140 | 4,077,584 / 4,113,274 |
| Burn and return | 1,735 / 1,772 | 4,163,814 / 4,213,254 |

The shared burn carries a definition Cell Dep and consumes the
representation. The CKB import
sizes include each run's actual checkpoint witness. Both paths
preserve the same logical identity and enforce the original
approval. An independent WASM verifier checked three execution
receipts and the escrow, non-membership and restored Cell proofs
on both paths.

The locked release workspace passed 476 tests with 19 ignored. The
standalone object fixture, Spore and native UDT settlement, and a
funded Jolteon handoff with historical Intercell delivery all passed
on the devnets. The rebuilt Stem node repeated the shared round
trip with an independent WASM proof check. Go proof tests and the
frozen CometBFT feature check passed. The first RISC-V build
reported a moved reference in the new resolver. The archive includes
that failure, the corrected build and both standalone shared runs.

[Standalone reproduction](OPERATIONS.md#exercise-standalone-ckb-metatype) ·
[Custody reproduction](OPERATIONS.md#exercise-stem-to-ckb-metatype-custody) ·
[Signed runs, raw proofs and build logs](benchmarks/2026-11-26-stem-metatype-shared.json.xz).

## Cap-bound fungible Metatype custody

Four Stem Jolteon replicas and CKB 0.210.0 exported 250 native units
subject to a 200-unit ceiling per Cell. The source UDT and independent
cap Script both ran on Stem. The corresponding adapters each ran
on CKB, where the fixture minted two 125-unit outputs and transferred
them as 175 and 75. Both chains rejected a balanced merge above the
ceiling and a transfer that stripped the bound cap.

The shared-definition sequence redeemed 150 first, leaving 100
native units in escrow and 100 wrapped units on CKB. The next
mailbox nonce redeemed the final 100. The CKB representation was
consumed, and the Stem escrow retained its Capacity in an empty
untyped Cell. CKB rejected an internally well-formed return with a
stale nonce. Stem rejected a replay of the first authenticated
return. The independent WASM SDK verified four execution receipts,
two initial escrow memberships, both original escrow spends,
both release memberships, the remaining escrow's spend, and the
final bond membership. The rebuilt node reached common height 35.

The rows compare the first import, transfer and 150-unit redemption.
Both Metatype layouts use the same CKB ELFs and logical Script set.
The specialized control has one wrapped Cell per transfer and a
different application rule set. Imports include the actual checkpoint
witness from each devnet.

| CKB operation | Specialized bytes / cycles | Inline Meta bytes / cycles | Shared Meta bytes / cycles |
| --- | ---: | ---: | ---: |
| Import 250 | 1,987 / 4,254,371 | 3,018 / 5,072,531 | 2,681 / 5,190,122 |
| Transfer 250 | 526 / 1,668,424 | 1,646 / 2,583,199 | 1,207 / 2,639,096 |
| Burn 150 | 1,542 / 3,413,509 | 2,178 / 4,382,641 | 1,977 / 4,449,597 |

The first wrapped outputs occupy 962 CKB inline and 822 CKB with
the shared definition included. The latter is 486 CKB for the
two outputs and 336 CKB for the definition. The specialized Cell
occupies 142 CKB. The shared sequence's second CKB burn takes
1,723 bytes and 4,171,054 cycles. The separate shared full-return
run burns all 250 units in one transaction and verifies both capped
native release outputs.

The consecutive return fixture rejected 22 cases, including
inflation, wrong import recipient, missing mailbox or shared
definition, escrow Capacity drain, underburn, insufficient or
malformed route conditions, and stale or reused messages. The
first fixture attempts omitted the native cap code from the
deposit. Another early attempt reached the first redemption but
crafted a stale nonce with an inconsistent successor counter.
The final fixture distinguishes the mailbox's sequence rejection
from that encoding error. The archive also contains the first
contract-build error and corrected results.

The locked release workspace passed 477 tests with 19 ignored.
Go proof tests and the frozen CometBFT build passed. The
real devnet regressions covered immutable Metatype shared custody,
the specialized UDT route, standalone Metatype UDT, Spore, and
funded Jolteon handoff with historical Intercell delivery.

[Reproduction](OPERATIONS.md#exercise-cap-bound-metatype-fungible-custody) ·
[Signed transactions, proofs and failed attempts](benchmarks/2026-11-26-stem-metatype-fungible.json.xz).

## Borrowed Metatype Manifest views

CKB 0.210.0 ran the prior and candidate RISC-V ELFs over the same
standalone fixture, with both inline Cell Data and shared definitions.
The pinned Stem CLI signed every transaction. Each split consumes two
asset Cells and creates three, with the same amounts and logical
groups in corresponding runs. The four cases ran in reverse order
after the first round. All recorded split cycles and byte sizes
repeated exactly.

| Groups in long set | Prior / candidate inline cycles | Prior / candidate shared cycles | Shared cycles saved | Inline / shared transaction bytes |
| --- | ---: | ---: | ---: | ---: |
| 2 | 2,406,107 / 2,419,890 | 2,471,775 / 2,418,039 | 2.2% | 2,326 / 1,553 |
| 4 | 3,202,085 / 3,202,731 | 3,449,826 / 3,243,911 | 6.0% | 3,066 / 1,598 |
| 8 | 5,170,970 / 5,172,112 | 5,867,291 / 5,021,355 | 14.4% | 4,546 / 1,614 |
| 16 | 9,969,918 / 9,950,088 | 12,384,121 / 9,023,302 | 27.1% | 7,506 / 1,646 |

Each local VM resolves the actual definition and indexes canonical
Script ranges once. The dispatcher, custody gate and adapters inspect
borrowed Script and state slices. The dispatcher still meters the
full expanded size before dispatch. Shared Cell Data, transaction
sizes and occupied Capacity follow the existing wire format.

Inline costs moved by at most 0.6%. The 16-group candidate shared
split used 9.3% fewer cycles than candidate inline. The candidate
mixed-layout two-asset settlement used 4,127,197 cycles, against
4,324,384 with the prior ELFs. All corresponding rejection sets
matched, including 19 shared cases. The expansion control still
rejected 79,059 encoded bytes expanding to 132,867 bytes. The
fixture also consumed and replaced a definition at a new OutPoint
and then finalized a split using its replacement. The archive
records actual issuance, split and settlement proofs for each run.

The release workspace passed 477 tests with 19 ignored. The RISC-V
contract build, Go proof tests and CometBFT benchmark build passed.
Real CKB devnet and four-replica Jolteon runs covered immutable
Metatype custody, capped fungible custody with consecutive 150
and 100 unit returns, standalone object approvals and their
Polytype comparison, specialized UDT, Spore, and a funded committee
handoff followed by delivery of a historical Intercell message.
Independent WASM runs verified the execution and Cell state
proofs for both Metatype custody paths. The first baseline start
failed because the devnet RPC listener port was occupied. The
successful retry and the failed start logs are both in the archive.

[Reproduction](OPERATIONS.md#exercise-standalone-ckb-metatype) ·
[Signed transactions, proofs and failed start](benchmarks/2026-11-26-stem-metatype-view.json.xz).

## Revisable Metatype objects across CKB

Revision `1112c23` ran the same Stem node binary with four local
Jolteon replicas and CKB 0.210.0 devnet. Stem created revision 0 and
approved revision 1 before export. CKB confirmed changes to revisions
2 and 4, then burned the wrapped object. Stem restored revision 4,
verified the existing Type Script set, and consumed the authenticated
return. Both Manifest layouts completed the full sequence.

| CKB transaction | Revisable inline bytes / cycles | Revisable shared bytes / cycles | Large return fixture, shared bytes / cycles |
| --- | ---: | ---: | ---: |
| Import revision 1 | 2,602 / 4,987,460 | 2,266 / 5,036,187 | 2,470 / 5,071,543 |
| Transfer and revise to 2 | 1,383 / 4,067,102 | 1,149 / 4,116,873 | 1,149 / 4,093,318 |
| Revise to 4 | 1,387 / 4,053,494 | 1,153 / 4,079,385 | 1,746 / 4,133,020 |
| Burn revision 4 | 1,749 / 4,147,369 | 1,786 / 4,188,351 | 2,379 / 4,196,232 |

Imports carry real finality-backed mailbox proofs. Their raw witness
data in these runs was 794, 692 and 896 bytes respectively. Transfer,
revision and burn each used 170 raw witness bytes. The isolated
local submission-through-mining samples spanned 311–420 ms in
the table, including the fixture's block generation cadence.
These values describe development-chain inclusion. The archive
preserves the individual samples and actual witness proofs.

The standard inline object occupies 564 CKB. The shared object
occupies 293 CKB and its reusable definition is funded separately.
The revisable imports drew the CKB mailbox from 1,000 to 3,300 CKB
using actual signed funding. That Capacity remains in the mailbox
and reserves the maximum permitted return payload. The larger
revision fixture grew the object content to 630 bytes. The original
Stem object held 8 CKB. Its return at 8 CKB failed the Storage Bond
check, and the authorized return at 13 CKB succeeded. The standard
fixture also rejected diverting one shannon from the original object.

The standard shared fixture exercised 33 rejection cases. The
inline run exercised 32 because it needs no shared dependency.
The checks include invalid creation revision, same-revision edits,
rollback, missing independent approval, edits during export,
underfunded CKB mailbox, stale return data on either chain, group
removal and message replay. An independent WASM verifier checked
creation, native revision, export and redemption execution proofs.
It decoded revision 1 and revision 4 from the proven Cells, verified
both Type Scripts and checked the conserved Capacity and exact top-up.

The immutable shared control preserved the three transaction sizes
and all 23 rejection cases from the prior ELFs. Its import, transfer
and burn used 5,069,382, 4,096,071 and 4,216,032 cycles. The frozen
baseline samples ranged from 5,066,427–5,078,003,
4,042,513–4,064,721 and 4,164,529–4,218,180 cycles. The current
specialized immutable comparison used 2,387, 886 and 1,523 bytes
at 4,302,499, 3,272,913 and 3,404,804 cycles respectively.
That fixture supports the dedicated immutable object route.

The archive retains the early compiler failure and both unsuccessful
long-state fixtures. The first run failed because the CKB mailbox
Cell could not hold the return record. The second reached Stem and
its policy-removal control failed the Storage Bond check before
reaching the intended script check. The final fixture funds that
control and verifies the two rejection conditions separately.

The locked release workspace passed 478 tests with 19 ignored.
The RISC-V contract build, Go proofs and frozen CometBFT feature
check passed. Devnet regressions covered cap-bound fungible
partial and final return, Spore, funded committee handoff with
historical Intercell delivery, all four CKB fee payouts, and
historical and live-at-anchor CKB references.

[Reproduction](OPERATIONS.md#exercise-stem-to-ckb-metatype-custody) ·
[Signed transactions, proofs, timings and failed runs](benchmarks/2026-11-26-stem-metatype-revision.json.xz).

## VM verification reuse across parent changes

Revision `8914ab0` records whether the executed Script Groups read
the parent Header or candidate height. Each cached result commits
the full Witness, resolved Cell contents and origins, declared
Headers and authenticated L1 references. Results that read the
parent through the original VM, Spawn or Exec still require that
parent. Every transaction is resolved against its current candidate
state. Cached cycles are charged in full.

The test fixture used four local Jolteon replicas, 64 fresh-socket
submitters, two VM workers per replica, a 250 ms proposal interval,
two billion cycles per block and a 12-second drain allowance.
The baseline was `04bccd6`. At each rate the binaries received
byte-identical signed workloads and genesis, with equal network
limits. The two 2,000 TPS pairs reversed the order.

| Planned TPS and revision | Written / admitted / common-visible TPS in the offer window | Confirmed by end / client drops | Common visibility p50 / p95 / p99 | Aggregate replica CPU |
| --- | ---: | ---: | ---: | ---: |
| 400 baseline | 400.00 / 399.86 / 390.50 | 11,200 / 0 | 0.548 / 0.689 / 0.719 s | 22.87 s |
| 400 cache reuse | 400.00 / 400.00 / 393.18 | 11,200 / 0 | 0.506 / 0.639 / 0.662 s | 18.83 s |
| 2,000 baseline | 1,446.54 / 1,446.50 / 1,375.79 | 34,717 / 13,283 | 1.360 / 1.808 / 1.934 s | 68.15 s |
| 2,000 cache reuse | 1,838.21 / 1,835.54 / 1,757.58 | 44,117 / 3,883 | 0.896 / 1.153 / 1.195 s | 65.59 s |
| 2,000 baseline repeat | 1,460.17 / 1,457.50 / 1,368.33 | 35,044 / 12,956 | 1.335 / 1.768 / 1.824 s | 67.41 s |
| 2,000 cache reuse repeat | 1,789.62 / 1,789.46 / 1,718.67 | 42,951 / 5,049 | 0.929 / 1.217 / 1.259 s | 68.93 s |
| 1,000 baseline, 60 s | 1,000.00 / 1,000.00 / 985.60 | 60,000 / 0 | 0.969 / 1.224 / 1.286 s | 127.41 s |
| 1,000 cache reuse, 60 s | 1,000.00 / 1,000.00 / 984.60 | 60,000 / 0 | 0.710 / 0.901 / 0.947 s | 107.20 s |

The 400 TPS runs offered traffic for 28 seconds. At the same
11,200 confirmed payments, aggregate replica CPU fell 17.7% and
mean candidate verification for uncached proposals fell from
13.29 to 9.37 ms. The four processes wrote 229.56 and 226.03 MB
and sent 14.61 and 14.46 MB of peer traffic. The 2,000 TPS runs
offered traffic for 24 seconds. Common visibility in the offer
window rose 27.8% and 25.6% across the two pairs. Including drain,
the two baseline rates were 1,405.79 and 1,413.12 TPS and the
cache-reuse rates were 1,789.20 and 1,731.84 TPS. Client drops
occurred before writing, and every completed write was confirmed.
Process writes rose from 471–480 MB to 677–686 MB as more payments
confirmed. Mean uncached candidate verification fell from
73.71–74.14 to 61.49–66.44 ms.

Both 60-second runs sustained 1,000 TPS written and admitted,
and confirmed all 60,000 payments. The candidate reduced aggregate
replica CPU by 15.9% and common-visibility p99 by 26.4%.
Process writes were 1,086.03 and 1,136.53 MB. The four replicas
sent 96.53 and 83.52 MB over peer connections and peaked at
814.0 and 809.3 MiB of aggregate resident memory. Including drain,
finalized throughput was 989.34 and 990.22 TPS. The two 2,000 TPS
pairs used 781–794 MiB of resident memory on the baseline and
801–813 MiB on the candidate at their higher actual admission rates.

The separate userspace profiles recorded 6,291 and 6,012 samples
with zero losses. The baseline profiling run reached only 456.37
TPS written, versus 1,735.67 for the candidate. Both recordings
are preserved. Their sample shares are descriptive because the
achieved loads differed substantially. The unprofiled comparisons
in the table are the throughput evidence.

The release workspace passed 479 tests with 19 ignored. Real
RISC-V controls use identical resolved inputs on distinct parents
and exercise the context syscall and the parent Header syscall
independently. They also test each read through Spawn or Exec.
Complete signed payments use the same cached cycles, while
removing a seal or lowering the cycle limit causes rejection.
The Go proof suite and frozen CometBFT feature check passed.

A mixed network with two binaries of each revision finalized 96
payments during a one-validator outage, then restarted the missing
validator. All four agreed on the complete execution Header and the
archive's two-chain finality proof. Separate real CKB devnets verified
a funded committee handoff with historical Intercell delivery, both
kinds of L1 Cell Reference, and the revisable Metatype round trip
with a Stem Capacity top-up.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Signed workloads, raw profiles, proof and recovery evidence](benchmarks/2026-11-26-stem-vm-parent-cache.json.xz).

## Live Cell reuse across candidate branches

Revision `6f15f7a` lets candidate branches resolve their decoded
Live Cell changes by OutPoint. A candidate deletion masks the
committed record. Branches share only committed cache entries.
Finality evicts the changed OutPoints and snapshot installation
clears that cache. Prepared blocks retain their decoded changes
alongside the serialized commit batch. The database format, atomic
write and authenticated record encoding are shared with the
`fead22e` baseline.

Four local Jolteon replicas used a pinned load driver, two VM
workers per replica, 64 fresh-socket submitters, a 250 ms proposal
interval and a two-billion-cycle block budget. Each comparison
used the same signed workload, genesis and limits. The equal
1,000 TPS payment pair ran for 36 seconds. Each planned 2,000 TPS
payment run and each distributed AMM run lasted 24 seconds.
Every run had a 12-second drain allowance.

| Workload and revision | Written / admitted / common-visible TPS in offer | Confirmed by end / client drops | Visibility p50 / p95 / p99 | Aggregate replica CPU |
| --- | ---: | ---: | ---: | ---: |
| Payments 1,000 baseline | 1,000.00 / 1,000.00 / 983.00 | 36,000 / 0 | 0.708 / 0.897 / 0.943 s | 69.85 s |
| Payments 1,000 reuse | 1,000.00 / 1,000.00 / 982.53 | 36,000 / 0 | 0.705 / 0.883 / 0.932 s | 69.86 s |
| Payments 2,000 baseline | 1,777.75 / 1,775.08 / 1,699.38 | 42,666 / 5,334 | 0.939 / 1.216 / 1.280 s | 70.08 s |
| Payments 2,000 reuse | 1,589.83 / 1,588.83 / 1,515.58 | 38,156 / 9,844 | 1.098 / 1.610 / 1.754 s | 76.51 s |
| Payments 2,000 baseline repeat | 1,727.62 / 1,726.46 / 1,669.04 | 41,463 / 6,537 | 0.989 / 1.373 / 1.510 s | 72.58 s |
| Payments 2,000 reuse repeat | 1,789.50 / 1,786.83 / 1,709.92 | 42,948 / 5,052 | 0.923 / 1.206 / 1.262 s | 67.65 s |
| Distributed AMM 140 baseline | 140.00 / 140.00 / 136.00 | 3,360 / 0 | 0.625 / 0.785 / 0.813 s | 22.77 s |
| Distributed AMM 140 reuse | 140.00 / 140.00 / 137.12 | 3,360 / 0 | 0.623 / 0.782 / 0.807 s | 22.37 s |

Every completed frame write was confirmed. The overload pairs
reversed their order and moved in opposite directions. Observed
throughput fell 10.8% in the first pair and rose 2.4% in the
second, with different achieved ingress rates. This round gives
no reproducible sustained throughput gain. At equal 1,000 TPS,
mean candidate verification changed from 42.95 to 41.90 ms.
Aggregate process writes were 583.05 and 575.46 MB, peer sends
were 48.08 and 46.96 MB, and peak aggregate resident memory was
668.7 and 679.2 MiB. The equal AMM pair used 256.6 and 263.3 MiB.
Its candidate verification mean changed from 23.28 to 22.64 ms.

Independent serial AMM replay verified 3,360 commands, 6,720
Head updates, identical total fees and cycles, each finalized
Header and receipt, and conservation of each market's assets
in both runs. Focused state tests warmed committed Live Cell
records before creating forks and installing a snapshot. They
verified distinct creation times for the same Output on
competing forks, deletion precedence through a child branch and
after finalization, and the absence proof for a spent record
after snapshot installation. The locked release workspace
passed 479 tests with 19 ignored. The Go proofs and frozen
CometBFT feature check passed.

A mixed four-replica network finalized 96 signed payments during
one validator outage and restarted the missing validator. The
replicas agreed on the complete execution Header and the archive's
finality proof. Separate real CKB devnets passed a funded committee
handoff and historical Intercell delivery, both kinds of L1 Cell
Reference, and the revisable Metatype return with Capacity top-up.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Signed workloads, raw timings, state replay and proofs](benchmarks/2026-11-26-stem-live-cell-reuse.json.xz).

## Deferred Live Cell record encoding

Candidate branches retain the decoded Live Cell changes needed for
execution. The Patricia leaf hashes the OutPoint, creation metadata
and canonical Cell bytes directly. During finality, one reusable
scratch buffer produces each created record in the atomic RocksDB
batch. The change removes a second representation of the created
record from the staged candidate.

The baseline was `69f79e0`. Revision `9691f16` initially placed
created records after the generic state mutations in the RocksDB
batch. Native signed workloads ran on four local Jolteon replicas
with two VM workers, 64 fresh-socket submitters, a 250 ms proposal
interval, a two-billion-cycle block budget and a 12-second drain.
Corresponding runs used identical signed workloads, genesis and
limits. The equal 1,000 TPS runs lasted 28 seconds, the planned
2,000 TPS and 140 TPS distributed AMM runs lasted 24 seconds, and
the first 2 KiB-output runs lasted 20 seconds.

| Sample | Written / admitted / common-visible TPS in offer | Confirmed / client drops | p50 / p95 / p99 | Replica CPU | Peak aggregate RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Payments 1,000 baseline | 1,000.00 / 1,000.00 / 977.25 | 28,000 / 0 | 0.649 / 0.815 / 0.856 s | 46.80 s | 620.4 MiB |
| Payments 1,000 initial | 1,000.00 / 1,000.00 / 980.32 | 28,000 / 0 | 0.643 / 0.806 / 0.849 s | 44.89 s | 619.2 MiB |
| Payments 2,000 baseline | 1,418.88 / 1,417.46 / 1,343.79 | 34,053 / 13,947 | 1.235 / 1.799 / 2.069 s | 76.08 s | 767.9 MiB |
| Payments 2,000 initial | 1,585.29 / 1,582.67 / 1,517.75 | 38,048 / 9,952 | 1.056 / 1.694 / 1.821 s | 76.74 s | 805.6 MiB |
| Payments 2,000 baseline repeat | 1,792.46 / 1,789.79 / 1,711.75 | 43,019 / 4,981 | 0.920 / 1.200 / 1.280 s | 68.56 s | 812.4 MiB |
| Payments 2,000 initial repeat | 1,788.96 / 1,786.29 / 1,705.08 | 42,935 / 5,065 | 0.917 / 1.193 / 1.283 s | 67.20 s | 835.8 MiB |
| 2 KiB payments 1,000 baseline | 988.30 / 986.55 / 937.05 | 19,766 / 234 | 0.908 / 1.263 / 1.358 s | 51.50 s | 753.3 MiB |
| 2 KiB payments 1,000 initial | 957.70 / 954.50 / 903.70 | 19,154 / 846 | 0.968 / 1.442 / 1.628 s | 51.90 s | 780.9 MiB |
| Distributed AMM 140 baseline | 140.00 / 140.00 / 136.33 | 3,360 / 0 | 0.625 / 0.787 / 0.812 s | 22.67 s | 261.4 MiB |
| Distributed AMM 140 initial | 140.00 / 140.00 / 136.08 | 3,360 / 0 | 0.636 / 0.818 / 1.001 s | 22.86 s | 261.9 MiB |

All written payments and AMM commands were visible by the end
of drain. The two overloaded pairs reversed ordering and reached
different actual ingress rates. Their results give no repeatable
overload speedup. The first 2 KiB comparison observed slower
RocksDB synchronization with deferred records, 7.85 to 11.46 ms
per write. It reached 988.30 and 957.70 TPS written. Both the
initial write order and the archived run data are useful for
reproducing this difference.

The two initial AMM runs each confirmed 3,360 commands with
6,720 Head updates and identical fees and cycles. Independent
serial replay verified every finalized Header and receipt,
including market balances and total asset conservation.

Revision `7d3fd94` restored the baseline RocksDB operation
order, with created records before the other batch entries. A
second ordinary-payment pair ran for 28 seconds with the candidate
first. A 24-second three-way comparison offered 750 payments per
second with 2 KiB per output. Its Cell Data used fixed repeated
bytes. The byte-size check decoded the actual signed transactions.

| Sample | Written / admitted / common-visible TPS in offer | Confirmed / client drops | p50 / p95 / p99 | Replica CPU | Peak aggregate RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Payments 1,000 baseline | 985.64 / 984.61 / 958.86 | 27,598 / 402 | 0.694 / 0.912 / 1.409 s | 49.80 s | 626.1 MiB |
| Payments 1,000 original write order | 990.68 / 990.68 / 970.39 | 27,739 / 261 | 0.673 / 0.861 / 1.177 s | 49.52 s | 584.7 MiB |
| 2 KiB payments 750 baseline | 748.25 / 748.25 / 730.83 | 17,958 / 42 | 0.620 / 0.792 / 0.875 s | 32.92 s | 697.1 MiB |
| 2 KiB payments 750 initial | 750.00 / 749.96 / 727.88 | 18,000 / 0 | 0.630 / 0.803 / 0.847 s | 32.96 s | 716.2 MiB |
| 2 KiB payments 750 original write order | 750.00 / 750.00 / 730.00 | 18,000 / 0 | 0.617 / 0.799 / 0.855 s | 34.77 s | 693.0 MiB |

RocksDB synchronization in the nearly equal 2 KiB comparison
averaged 7.07 ms on the baseline, 7.48 ms in the initial revision
and 4.77 ms with the original write order. End-to-end certified
commit averaged 7.96, 8.40 and 5.90 ms respectively. The final
revision used 5.6% more aggregate CPU than the baseline in that
single near-equal 24-second comparison. Common visibility and
resident memory were roughly level between those two runs.
Their process writes were 807.17 and 803.21 MB and peer sends
were 208.86 and 202.19 MB. Persisted Live Cell batch bytes per
payment per replica were 2,276 across all three 2 KiB runs,
including the spent record deletion.

The ordered 1,000 TPS run peaked at 584.7 MiB across four
replicas, versus 626.1 MiB for its paired baseline at a slightly
lower achieved ingress rate. The ordinary pair had almost equal
CPU time, and mean RocksDB synchronization went from 5.48 to
6.50 ms. These samples do not establish a general latency,
RSS or TPS improvement.

The byte accounting derives directly from the canonical record
and from observed block sizes. Each ordinary output record is
154 bytes. Each 2 KiB output record is 2,202 bytes. For the
304-payment block observed in the final 750 TPS run, the prior
candidate would retain 669,408 bytes of additional encoded
record values. These bytes describe candidate representation.
The process RSS samples include execution, caches and the
database and are reported separately.

The locked release workspace passed 480 tests with 19 ignored.
A byte-level control checks the old field order, streaming hash,
scratch buffer reuse across sizes and decoding of the persisted
record. The RISC-V contracts were rebuilt from the updated
commitment code. Go proofs and the frozen CometBFT feature
check passed.

A mixed four-replica network finalized 96 payments with one
validator offline, then recovered and produced the same execution
Header and finality proof on both revisions. With rebuilt RISC-V
contracts, real CKB devnet accepted a funded committee handoff
and historical Intercell message, historical and live-at-anchor
CKB references, and the revisable Metatype return with a signed
Capacity top-up.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Signed workloads, ordered and initial timings, byte accounting and recovery evidence](benchmarks/2026-11-26-stem-live-cell-finality.json.xz).

## Fixed-buffer Patricia reads

The baseline is `87b5730` and the candidate is `fc49e54`. Both read
the same compact Patricia records and check their canonical hashes.
The candidate reconstructs up to 98 bytes inline and borrows nodes
already staged by the editor. Proof generation still produces the
canonical path bytes.

The RocksDB editor probe seeds 8,192 records, then removes 512 and
inserts 512 per trial. The two versions alternated in baseline,
candidate, candidate, baseline order, with 11 warm compact samples
per invocation. Each compact sample read 4,120 nodes and 277,207
value bytes and generated a 503,521-byte node batch. The resulting
root and exact edit operations matched.

| Median across 22 compact samples per version | Baseline | Fixed buffer |
| --- | ---: | ---: |
| Canonical reconstruction per edit | 0.124 ms | 0.083 ms |
| RocksDB fetches per edit | 2.033 ms | 2.047 ms |
| Complete edit | 5.741 ms | 5.672 ms |

The reconstruction time fell 32.8%. The complete edit was 1.2%
faster in this probe. The node batch had the same size.

The payment comparison uses four colocated Jolteon replicas, two
VM workers per replica, 64 senders, a 250 ms proposal interval, a
two-billion-cycle block budget and a 12-second drain. The 1,000
TPS target ran for 24 seconds, baseline first. The 2,000 TPS
target ran for 20 seconds, candidate first. Each pair used the
same genesis, resource limits and signed transactions.

| Target and revision | Written / admitted / common-visible TPS in offer | Confirmed / client drops | p50 / p95 / p99 | Replica CPU | Peak aggregate RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1,000 baseline | 1,000.00 / 1,000.00 / 976.88 | 24,000 / 0 | 0.646 / 0.805 / 0.834 s | 40.43 s | 587.3 MiB |
| 1,000 fixed buffer | 1,000.00 / 1,000.00 / 978.00 | 24,000 / 0 | 0.645 / 0.812 / 0.848 s | 40.30 s | 591.9 MiB |
| 2,000 baseline | 1,798.40 / 1,795.20 / 1,704.25 | 35,968 / 4,032 | 0.922 / 1.211 / 1.263 s | 57.22 s | 727.1 MiB |
| 2,000 fixed buffer | 1,796.65 / 1,794.15 / 1,707.35 | 35,933 / 4,067 | 0.929 / 1.196 / 1.284 s | 57.52 s | 722.7 MiB |

Confirmation throughput including drain was 978.82 and 978.36 TPS
in the first pair and 1,741.11 and 1,729.72 TPS in the second.
Aggregate process writes were 402.52 and 402.28 MB, then
483.96 and 488.06 MB. Peer sends were 29.99 and 29.64 MB, then
65.36 and 64.67 MB. All offered transactions were confirmed.
The 2,000 TPS runs reached roughly 1,798 TPS at the client.
The run pairs did not establish a sustained throughput or p99
latency gain from the fixed buffer.

The release workspace passed 480 tests with 19 ignored. Go proof
tests and the frozen CometBFT feature check passed. A mixed
four-replica Jolteon network confirmed 96 payments with one
validator offline and recovered to a common execution Header.
A CKB devnet accepted the funded four-to-four committee handoff
and a historical Intercell delivery. Separate CKB runs verified
historical and live-at-anchor Cell references and a revisable
Metatype custody round trip with a signed Capacity top-up.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw editor samples, signed workloads, build fingerprints and CKB evidence](benchmarks/2026-11-26-stem-patricia-inline-read.json.xz).

## Reusing RocksDB point-read options

Revision `9c04e95` shares one immutable default `ReadOptions`
between the committed Store and its candidate views. The default
point-read path at `186707f` constructs an options handle for each
committed lookup. Both revisions check candidate batches before
consulting RocksDB.

The editor fixture starts with 8,192 records and replaces 512 per
trial. Each trial reads 4,120 nodes and 277,207 stored bytes. It
checks the resulting root and every write against an in-memory
edit. The timings below combine 42 retained samples per method
and storage state from the two final invocations. Each invocation
rotates the three read methods, discards three warm-up rounds and
flushes the same database before measuring its warmed SST.

| Storage | Read method | Point reads per edit, median | Full edit, median |
| --- | --- | ---: | ---: |
| Memtable | Default `get` | 1.916 ms | 5.530 ms |
| Memtable | Reused options `get_opt` | 1.892 ms | 5.510 ms |
| Memtable | Pinned values `get_pinned_opt` | 1.831 ms | 5.486 ms |
| Warm SST | Default `get` | 3.370 ms | 7.113 ms |
| Warm SST | Reused options `get_opt` | 3.374 ms | 7.054 ms |
| Warm SST | Pinned values `get_pinned_opt` | 2.892 ms | 6.775 ms |

Sharing the default options removes one options allocation per
committed lookup. The median full edit improved by 0.4% in the
memtable probe and by 0.8% in the warm SST probe. The warm SST
full-edit medians for pinned values were 7.192 and 7.187 ms for
pinned and default reads in the exploratory invocation, versus
6.775 and 7.113 ms in the two final runs. The node comparison keeps
owned results and measures the options change alone. All three
probe invocations are archived.

The network comparison ran four local Jolteon replicas with
64 senders, two VM workers per replica, a 250 ms proposal
interval and a two-billion-cycle block budget. The 1,000 TPS
workload used fresh connections for a 22-second offer window
and a 12-second drain limit. The 2,600 TPS workload reused
connections for 32 seconds with an 18-second drain limit. Each
pair matched the signed workload, genesis and resource limits.

| Offer target and revision | Written / admitted / common-visible TPS in offer | Finalized TPS with drain | Confirmed / client drops | p50 / p95 / p99 | Replica CPU | Peak aggregate RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 baseline | 1,000.00 / 1,000.00 / 970.86 | 981.83 | 22,000 / 0 | 0.640 / 0.807 / 0.853 s | 36.79 s | 579.6 MiB |
| 1,000 reused | 1,000.00 / 1,000.00 / 972.86 | 979.46 | 22,000 / 0 | 0.642 / 0.810 / 0.846 s | 37.31 s | 577.4 MiB |
| 2,600 baseline | 1,750.97 / 1,748.97 / 1,654.41 | 1,669.39 | 56,031 / 27,169 | 1.270 / 1.759 / 5.547 s | 108.89 s | 956.4 MiB |
| 2,600 reused | 1,879.50 / 1,878.59 / 1,817.56 | 1,805.15 | 60,144 / 23,056 | 1.170 / 1.622 / 3.118 s | 102.40 s | 958.1 MiB |
| 2,600 reused, reverse pair | 906.81 / 904.81 / 853.28 | 883.75 | 29,018 / 54,182 | 1.763 / 2.286 / 2.444 s | 117.33 s | 902.8 MiB |
| 2,600 baseline, reverse pair | 1,580.88 / 1,579.28 / 1,499.84 | 1,550.75 | 50,588 / 32,612 | 1.355 / 1.811 / 3.301 s | 112.61 s | 940.0 MiB |

All written transactions confirmed within the observed drain.
In the first 2,600 TPS pair, the baseline and candidate ended
the offer window with 3,090 and 1,982 written transactions
awaiting common visibility. Their aggregate process writes
were 991.06 and 1,028.42 MB. RocksDB counted 536.01 and
574.09 MB of WAL activity. Peer sends were 104.10 and 112.15 MB.
The 1,000 TPS pair wrote 374.62 and 368.58 MB and sent 27.44
and 27.25 MB to peers.

A separate test process started before the reverse pair and
used roughly eight to nine cores' worth of CPU when sampled
during the pair. The archive retains both samples, the host
process snapshot, and cumulative offer, admission and visibility
counts at each eight-second boundary. Across both pairs,
roughly 99.8% or more of written transactions were admitted
within the offer window. Lower achieved submission rates and
host contention limit any attribution of throughput or p99
differences to the read options. The matching 1,000 TPS pair
shows essentially unchanged throughput.

The release workspace passed 480 tests with 20 ignored. Go
proof tests and the frozen CometBFT feature check passed. A
mixed four-replica Jolteon network confirmed 96 payments with
one validator offline and agreed on the recovered Header.
Real CKB devnets verified a funded committee handoff and
historical Intercell delivery, historical and live-at-anchor
Cell references, and a revisable Metatype return with a signed
Capacity top-up.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw point reads, signed loads, contention evidence and CKB proofs](benchmarks/2026-11-26-stem-patricia-point-reads.json.xz).

## Serial admission dispatch

At `92d94d9`, serial checks use a blocking Rayon handoff for
each submitted or relayed transaction. Revision `bbb21cf` runs
the same check on the replica thread. The admission order,
full witnessed identity, signed authorizations, protocol checks
and block-execution worker pool are unchanged.

The fixed-tip probe runs 512 signed AMM commands per trial.
Each command touches two Lineage Heads and executes the real
RISC-V contracts. Three stores start from the same Header.
The probe rotates single-request pool dispatch, direct serial
checks and groups of four using a two-worker pool. All three
produce identical receipts. Each of three invocations discards
one warm-up sample for the aggregates and retains it in the archive.

| Check dispatch | Median for 512 commands |
| --- | ---: |
| Blocking pool handoff for each command | 52.412 ms |
| Direct serial calls | 49.615 ms |
| Parallel groups of four | 28.137 ms |

Direct serial calls reduced the median by 5.34%, equivalent
to 5.46 μs per command in this probe. The batched timing is
a fixed-tip control. The earlier network experiment evaluated
its runtime tradeoffs separately.

The network runs use four local Jolteon replicas, 64 submitters,
two VM workers per replica, a 250 ms proposal interval, a
two-billion-cycle block budget and a 12-second drain limit.
The 1,000 TPS payment profile uses fresh sockets for 22 seconds.
The 2,400 TPS payment profile reuses connections for 24 seconds.
The distributed AMM profile offers 140 commands per second for
20 seconds. Each matched comparison pins the signed workload,
genesis and resource limits. Host process and pressure samples
are recorded every eight seconds.

| Profile | Written / admitted / common-visible TPS in offer | Finalized TPS with drain | Confirmed / client drops | Visibility p50 / p95 / p99 | Replica CPU / peak RSS | Process writes / peer sends |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 baseline | 1,000.00 / 1,000.00 / 970.82 | 981.31 | 22,000 / 0 | 0.644 / 0.811 / 0.845 s | 36.99 s / 579.9 MiB | 366.43 / 28.71 MB |
| 1,000 direct | 1,000.00 / 999.95 / 964.50 | 978.60 | 22,000 / 0 | 0.688 / 0.955 / 1.064 s | 40.78 s / 583.4 MiB | 373.71 / 29.91 MB |
| 1,000 direct, reverse | 454.41 / 451.50 / 405.36 | 424.20 | 9,997 / 12,003 | 2.488 / 3.762 / 4.978 s | 46.50 s / 409.3 MiB | 155.39 / 20.86 MB |
| 1,000 baseline, reverse | 428.91 / 426.00 / 380.64 | 406.11 | 9,436 / 12,564 | 2.361 / 3.526 / 4.438 s | 45.88 s / 407.5 MiB | 156.36 / 19.66 MB |
| 2,400 direct | 739.46 / 736.79 / 690.12 | 702.73 | 17,747 / 39,853 | 1.829 / 3.478 / 4.888 s | 58.87 s / 763.4 MiB | 336.52 / 37.13 MB |
| 2,400 baseline | 376.54 / 373.88 / 321.75 | 357.36 | 9,037 / 48,563 | 2.554 / 3.648 / 4.565 s | 46.67 s / 598.1 MiB | 167.49 / 18.38 MB |
| 2,400 baseline, reverse | 371.00 / 368.33 / 336.04 | 344.50 | 8,904 / 48,696 | 2.544 / 4.106 / 4.717 s | 48.63 s / 598.4 MiB | 166.74 / 18.97 MB |
| 2,400 direct, reverse | 469.54 / 466.88 / 412.00 | 429.08 | 11,269 / 46,331 | 2.751 / 4.076 / 5.246 s | 51.29 s / 641.5 MiB | 178.83 / 27.49 MB |
| AMM baseline | 102.15 / 97.10 / 34.60 | 84.23 | 2,043 / 757 | 11.054 / 16.790 / 17.769 s | 50.97 s / 214.7 MiB | 62.64 / 17.31 MB |
| AMM direct | 96.20 / 90.10 / 42.75 | 72.41 | 1,924 / 876 | 11.720 / 17.063 / 19.003 s | 55.59 s / 220.0 MiB | 48.19 / 21.66 MB |

Both initial 1,000 TPS runs wrote and finalized all 22,000
payments. The candidate's window overlapped a separate compiler
process. An independent checker used roughly 17–18 cores' worth
of CPU during the later experiments. Aggregate host activity
stayed near 20 active cores before the reverse-order runs.
Both 2,400-target pairs recorded higher actual admission and
common-visible throughput on the candidate, alongside a higher
p99 in each pair. The differences in offered traffic and host
activity prevent attribution of those whole-network results to
the dispatch change. The two AMM runs passed serial audits of
every finalized receipt and both Head updates per command.

The release workspace passed 480 tests with 20 ignored, including
concurrent submission of signed and unsigned variants.
Go proof tests and the frozen CometBFT feature check passed. A
mixed four-replica network confirmed 96 payments with one
validator offline and recovered to the same Header. Real CKB
devnets verified the funded committee handoff and historical
Intercell delivery, historical and live-at-anchor Cell references,
and the Metatype return with a signed Capacity top-up.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw checks, signed workloads, per-run host samples and CKB proofs](benchmarks/2026-11-26-stem-inline-admission.json.xz).

## Stake Registry loading during admission

Revision `cd488ea` loads the staged Stake Registry only for a
Stake Snapshot during single-transaction admission. Revision
`754b1ef` loads it before each admission check. Block preparation
still loads staking state for height-driven committee and reward
updates, including blocks with no Stake Snapshot.

The fixed-tip probe checks 128 distinct signed payments against a
stopped Stake-backed Jolteon replica. The payments vary the fee
and each independently spends the same live Capacity output from
a confirmed CKB deposit. They exercise individual admission on one
committed view. A single block cannot include the conflicting
payments together. The test compares complete receipts on each
repetition and across revisions. Four alternating process runs
reopen the Store for each trial, time the first pass with a fresh
Store verification cache, then time an identical pass using that
cache. The process-local VM caches can persist across trials.

| Funded committee | Tip height | First pass baseline / candidate | Repeated pass baseline / candidate |
| --- | ---: | ---: | ---: |
| 4 members | 55 | 19.748 / 19.995 ms | 1.655 / 1.295 ms |
| 128 members | 57 | 21.320 / 20.909 ms | 2.020 / 1.321 ms |

Each cell in the table is the median of 24 batches after excluding
the first trial of each process. The repeated checks reduced
median time by 21.8% for four members and 34.6% for 128 members.
The first passes moved by +1.25% and −1.92%, respectively. These
are Store check timings. The live regression covers validity,
committee activation and recovery.

An observer restored the four-member probe state from a pinned
locally exported Header ID, snapshot and Jolteon certificates.
It reproduced the exact Header and receipt digest. Both probe
states come from local networks with CKB-confirmed deposits.
The raw archive includes both authenticated snapshots, guardian
observations, signed workloads and finality certificates.

The release workspace passed 480 tests with 21 ignored. The stake
test admits a valid snapshot at its signing anchor, compares the
admission receipt with block preparation, and rejects stale or
unauthorized snapshots. Go proof tests and the frozen CometBFT
feature check passed. The mixed four-replica test finalized 96
signed payments through an offline validator and recovery.

Real CKB devnets confirmed four- and 128-member committee
handoffs and accepted historical Intercell deliveries. The wide
case used 129 Stem processes, applied the handoff at height 40,
and agreed at height 44 after one continuing voter restarted.
Separate devnets verified historical and live-at-anchor Cell
references and the revisable Metatype return. The archive
retains the initial probe-launch and signing-context failures.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw timings, both funded snapshots and CKB proofs](benchmarks/2026-11-26-stem-staked-admission.json.xz).

## Reusing decoded stake inputs across blocks

Revision `183c7dd` reuses the decoded accepted Stake Snapshot,
committee, applied Registry sequence and guardian authority.
A write to any of those four authenticated keys starts a new
cache for the resulting state view. Revision `88fac89` loads
and decodes the records for each block preparation. Both revisions
stage a fresh budget and mutable changes per preparation.

The probe reopens the same CKB-funded four-member and 128-member
archive states at heights 55 and 57. Each trial prepares 128 empty
blocks or 128 separate one-payment blocks from the same tip. A third
run prepares empty children of a candidate payment block that has
advanced the fee tally and SystemRoot. The signed payment variants
share a funded input, and each runs in its own preparation. The probe
compares the complete result Header and receipt digest for every
profile across both binaries. The medians use 24 trials for each
version and profile after discarding the first trial of each process.

| Committee and profile | Baseline | Cached stake inputs | Time reduction |
| --- | ---: | ---: | ---: |
| 4, empty | 1.182 ms | 0.795 ms | 32.7% |
| 4, one payment | 5.944 ms | 5.534 ms | 6.9% |
| 4, candidate child | 1.225 ms | 0.852 ms | 30.5% |
| 128, empty | 1.329 ms | 0.819 ms | 38.4% |
| 128, one payment | 5.918 ms | 5.445 ms | 8.0% |
| 128, candidate child | 1.489 ms | 0.945 ms | 36.6% |

The timings cover in-process preparation at a fixed state. The
sibling branch test verifies that a candidate accepting the snapshot
rejects replay, while the committed parent and an unchanged sibling
still accept it. It also primes an observer's genesis stake cache
before restoring a later snapshot and checks the rotated authority
and retirement state.

The locked release workspace passed 480 tests with 22 ignored.
Go proof tests and the frozen CometBFT feature check passed. A
mixed four-replica network finalized 96 signed payments and
recovered the replica that was offline. Real CKB devnets accepted
the four-member and 128-member committee handoffs at Stem height
40 and both historical Intercell deliveries. The wide run used
129 Stem processes and agreed at height 44 after restarting an
offline voter. Separate runs verified historical and live-at-anchor
Cell references and the revisable Metatype custody round trip.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw preparations, signed workloads, state images and CKB proofs](benchmarks/2026-11-26-stem-stake-view-cache.json.xz).

## Jolteon frame writes

Revision `2604334` submits a frame's fixed four-byte length and
body together. The asynchronous replica writes use two I/O
slices and handle partial writes through Tokio. The blocking
Rust client encodes into a length-prefixed request buffer.
Revision `e3bb2aa` writes the same two parts separately.

A loopback test used the asynchronous writer at both ends of
one session and exchanged 256 sequential requests and replies.
Tracing `sendto`, `sendmsg` and `writev` counted 1,024
`sendto` calls on the baseline and 512 `writev` calls on the
candidate, with zero errors. A separate TCP test sends a 4 MiB
frame and then verifies that the next frame still decodes.

The four-replica tests use the same signed payments, genesis,
250 ms proposal interval, two VM workers per replica, two-billion
cycle block budget and 64 submitters. The 1,000 TPS runs use fresh
sockets for 20 seconds. The 2,000 TPS runs use connection reuse
for 22 seconds. The high profile isolates the node by also running
the new replica binary with the old Rust client fixture. The raw
report includes per-run host and pressure samples.

| Profile | Written / admitted / common-visible TPS in offer | Finalized TPS with drain | Confirmed / client drops | Visibility p50 / p95 / p99 | Replica CPU / peak RSS | Process writes / peer sends |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 baseline | 998.00 / 997.25 / 949.25 | 934.79 | 19,960 / 40 | 0.725 / 1.100 / 1.247 s | 36.34 s / 513.7 MiB | 363.19 / 24.99 MB |
| 1,000 node + client | 987.70 / 987.70 / 960.70 | 964.44 | 19,754 / 246 | 0.702 / 1.026 / 1.208 s | 32.03 s / 506.5 MiB | 430.73 / 26.39 MB |
| 2,000 baseline | 1,778.23 / 1,777.95 / 1,682.45 | 1,732.94 | 39,121 / 4,879 | 0.904 / 1.170 / 1.227 s | 54.10 s / 735.9 MiB | 632.16 / 70.80 MB |
| 2,000 node only | 1,746.41 / 1,744.86 / 1,651.73 | 1,700.37 | 38,421 / 5,579 | 0.928 / 1.264 / 1.437 s | 56.73 s / 737.2 MiB | 609.23 / 66.76 MB |
| 2,000 node + client | 1,794.82 / 1,793.86 / 1,703.55 | 1,750.10 | 39,486 / 4,514 | 0.898 / 1.177 / 1.327 s | 54.93 s / 743.8 MiB | 682.96 / 69.12 MB |
| 2,000 baseline, repeat | 1,657.73 / 1,657.68 / 1,588.36 | 1,614.95 | 36,470 / 7,530 | 1.042 / 1.369 / 1.441 s | 66.00 s / 746.0 MiB | 490.59 / 67.78 MB |
| 2,000 node + client, repeat | 1,384.82 / 1,381.91 / 1,333.05 | 1,311.53 | 30,466 / 13,534 | 1.146 / 2.313 / 2.815 s | 65.57 s / 745.2 MiB | 437.91 / 56.26 MB |

The combined change increased offer-window common visibility by 1.3%
in the first 2,000 TPS pair and decreased it by 16.1% in the reverse-order
pair. Visibility p99 increased in both pairs. The repeat reached a
lower actual offer rate in both versions and overlapped another Rust
build on the host. These runs leave the end-to-end performance
effect unresolved.

The release workspace passed 481 tests with 22 ignored. Go proof
tests and the frozen CometBFT feature check passed. The mixed-version
four-replica network finalized 96 signed payments and recovered its
offline replica. Separate CKB devnets verified Cell references and
the revisable Metatype custody round trip.

The first 128-member handoff run stopped when the host filesystem
filled during RocksDB writes. After free space recovered, a fresh run
on different ports completed with 129 Stem processes and confirmed
the handoff on CKB at Stem height 40. The replica that restarted caught
up to the agreed Header at height 44. Both the four-member and
128-member handoffs delivered historical Intercell messages on CKB.
The archive retains the first failed build, the disk-full run and the
successful retry.

[Reproduction](OPERATIONS.md#measure-native-storage-and-sustained-payments) ·
[Raw traces, signed workloads, logs and CKB proofs](benchmarks/2026-11-26-stem-framed-writes.json.xz).

## Borrowed transaction output validation

Revision `430306a` validates output Scripts in the verified Strand
byte ranges. It avoids constructing temporary `Cell` and packed
`Script` objects during transaction decoding. For one input or Cell
Dep it still validates the entry and skips the uniqueness table.
Duplicate checks apply whenever a collection has multiple entries.
The encoded limits, signatures, Script Groups, execution and resource
accounting retain their existing rules.

The decoder probe ran `Transaction::from_slice` over the same 512
signed payments and 512 signed Lineage commands used in earlier
network experiments. Each variant ran 15 samples of 65,536 complete
decodes pinned to CPU 4. The table reports median calling-thread CPU
time and separately counted heap allocations, including reallocations.
A concurrent job affected wall-clock timing. The archive includes the
wall-clock controls, both library build fingerprints and fixture IDs.

| Signed fixture | Base `928c0bf` CPU per decode | `430306a` CPU per decode | Heap allocations per decode |
| --- | ---: | ---: | ---: |
| Ordinary payment | 3.506 µs | 2.695 µs | 17 → 9 |
| Lineage command | 11.311 µs | 11.606 µs | 16 → 14 |

The median CPU cost for payment decoding fell 23.1%. The Lineage
command has no outputs and showed a 2.6% higher median CPU time in
this probe. Two fewer allocations did not produce a measured CPU
improvement for that fixture.

The four-replica trials used the same signed workload and genesis
within each rate, a 24-second offer window, two VM workers per node,
a 250 ms cadence, 2 billion block cycles and persistent submission
connections. All transactions written by the client were eventually
confirmed in every run. Replica CPU and output bytes were counted
through drain.

| Pair and build | Planned TPS | Written / admitted / all-replica visible TPS during offer | Confirmed incl. drain | p99 | Replica CPU s | Process writes MB |
| --- | ---: | --- | ---: | ---: | ---: | ---: |
| Initial base | 1,000 | 428.71 / 426.04 / 379.88 | 10,289 | 5.370 s | 49.52 | 154.67 |
| Initial candidate | 1,000 | 387.17 / 384.50 / 343.29 | 9,292 | 5.038 s | 46.34 | 150.18 |
| Initial base | 2,000 | 1,393.71 / 1,391.46 / 1,303.71 | 33,449 | 1.746 s | 84.22 | 466.25 |
| Initial candidate | 2,000 | 1,108.50 / 1,105.83 / 1,027.21 | 26,604 | 2.388 s | 88.27 | 420.56 |
| Reverse-order base | 1,000 | 1,000.00 / 1,000.00 / 970.38 | 24,000 | 0.961 s | 46.53 | 403.01 |
| Reverse-order candidate | 1,000 | 1,000.00 / 1,000.00 / 963.08 | 24,000 | 0.927 s | 45.30 | 407.43 |
| Reverse-order base | 2,000 | 1,650.88 / 1,648.21 / 1,573.17 | 39,621 | 1.425 s | 70.76 | 633.87 |
| Reverse-order candidate | 2,000 | 1,625.38 / 1,622.71 / 1,543.38 | 39,009 | 1.542 s | 73.38 | 629.23 |

The first four runs overlapped with a separate CPU-intensive job.
Its worker count fell from 21 to seven by the start of the initial
2,000 TPS base run, then to two or three in the reverse-order runs.
The quieter 1,000 TPS pair executed the same 24,000 payments and
recorded 2.6% lower replica CPU on the candidate. At the 2,000 TPS
repeat the candidate confirmed 1.9% fewer transactions per second in
the offer window while the client wrote 1.5% fewer. The retained
optimization reduces the cost of decoding ordinary payments. The
network trials establish no throughput gain.

The release workspace passed 483 tests with 22 ignored. Regression
tests cover accepted Scripts at the exact size limit, rejected
oversized Lock and Type Scripts and duplicate Cell references.
A mixed-build four-node run confirmed 96 payments with one validator
offline and recovered to a common state. Real CKB devnets verified a
funded committee handoff and historical Intercell delivery, both L1
Cell Reference modes, and revisable Metatype custody including a 5 CKB
top-up and proofs of the restored object. The Go proof suite and
frozen CometBFT build check passed.

[Raw results, build fingerprints, proofs and reproduction commands](benchmarks/2026-11-26-stem-transaction-decode.json.xz).

## Three-constraint Metatype with an external rule

Four Jolteon replicas and a real CKB devnet executed an object with
independent identity, approval and state-length groups. A two-byte
record gives the ceiling on encoded state. Both hosts resolved that
record by its CKB data hash in the actual Cell Deps. A signed revision
from the same state to the same oversized payload was rejected on
both chains. Each rejected a missing record, a different record,
removal of the rule and changing the rule Script to point to a looser
record. The CKB publisher spent the original record and published
the same two bytes at a new OutPoint. A later update and burn used
that replacement. Each chain also rejected a return step without
the rule dependency. Stem restored the exact authorized state at
the byte limit and retained all three original Scripts. The dedicated
bridge rejected an export with an additional constraint.

The two-constraint controls used the same build and revisable object
flow with identity and approval. The dedicated immutable route is
shown as a separate reference workload. These are individual CKB
transactions measured with `estimate_cycles` and committed on the
devnet. Occupied Capacity refers to the wrapped object at import.
The rule's separate two-byte Cell occupies at least 63 CKB under the
standard Lock. The shared definition Cells used 369 CKB with two
Scripts and 490 CKB with three. These figures exclude code, escrow
and Mailbox reserves.

| Profile and layout | Object occupied CKB | Import / transfer / burn cycles | Signed bytes import / transfer / burn | Transfer Witness bytes |
| --- | ---: | --- | --- | ---: |
| Dedicated immutable | 256 | 4,248,758 / 3,323,607 / 3,384,466 | 2,081 / 886 / 1,523 | 170 |
| Two constraints, inline | 564 | 4,981,182 / 4,076,642 / 4,142,713 | 2,602 / 1,383 / 1,749 | 170 |
| Three constraints, inline | 685 | 5,319,860 / 4,458,602 / 4,491,863 | 3,001 / 1,578 / 1,864 | 170 |
| Two constraints, shared | 293 | 5,100,428 / 4,088,030 / 4,181,850 | 2,572 / 1,149 / 1,786 | 170 |
| Three constraints, shared | 295 | 5,406,127 / 4,480,808 / 4,557,259 | 2,546 / 1,225 / 1,901 | 170 |

The added constraint increased CKB transfer cycles by 9.4% inline and
9.6% with shared Manifests, with the same transfer Witness size. The
three-constraint transfers took about 416 and 419 ms respectively from
submission through local mining. Some other work overlapped the tests.
The files retain individual finality proofs, timing and signed byte
measurements. Checkpoint message proofs can vary in length across runs.

An independent WASM verifier checked the original Stem rule Cell,
execution receipts, escrow membership and absence, and the restored
object. It verified revision 4 with all three Scripts in both storage
layouts. A third verification used a 700-byte final state, restoring
9 CKB of original Capacity and 5 CKB of authorized top-up. Missing that
top-up was rejected. Two earlier top-up runs hit ordinary Storage Bond
validation before the intended check. The retry funds the oversized
negative case to isolate the state policy and gives the three-Script
escrow enough initial Capacity to carry the object. The report also
contains the initial WASM build failure and the successful retry with
the pinned bindgen.

The workspace passed 484 tests with 22 ignored. CKB regressions
covered 2, 4, 8 and 16 standalone constraints, fungible Metatype
export and return, Spore ID preservation, funded committee handoff
with Intercell delivery, four fee payouts, the CKB and xUDT bridge,
and both L1 Cell Reference modes. The Go proof suite and frozen
CometBFT build check passed.

[Signed transactions, proof inputs, measurements, failed runs and reproduction commands](benchmarks/2026-11-26-stem-metatype-external-rule.json.xz).

## Live Metatype rule registry

Four Jolteon replicas and a real CKB devnet exercised the same
three-constraint object with a Type ID registry. While the object
was held on CKB, the issuer increased its ceiling from 120 to 200
bytes. The registry maintained its Type and Lock across the update.
CKB admitted a larger revision after the update. Stem advanced its
committed CKB anchor, verified two separately certified references,
and restored the 200-byte revision with the three original Type
Scripts and the original Capacity. WASM verified the restored Cell
and execution receipts against native finality.

The signed controls exercised a forged Type ID creation, a lower
or unchanged ceiling, registry removal and replacement of its Lock.
Both hosts rejected missing or substituted registry data. Stem
rejected a historical observation offered as a live rule, an old
live observation at the new anchor, and a fresh request to attest
the consumed registry Cell as live. Removal of the rule Script and
return without its dependency were also rejected.

| Run | CKB transfer bytes | CKB transfer cycles | CKB burn cycles | Stem return bytes | Stem return cycles |
| --- | ---: | ---: | ---: | ---: | ---: |
| Fixed data hash, shared | 1,225 | 4,498,548 | 4,543,735 | 2,997 | 14,198,809 |
| Live registry, inline repeat | 1,616 | 4,432,217 | 4,493,720 | 3,747 | 26,153,686 |
| Live registry, shared retry | 1,262 | 4,483,859 | 4,567,701 | 3,747 | 26,153,686 |

The CKB registry update is 475 bytes and consumed 1.64–1.68 million
cycles across the successful runs. CKB transfer witnesses remained
170 bytes. The live custody fixture uses a 50-million-cycle Stem
transaction budget. Three additional guardian signatures account
for the 12-million-cycle fixed charge on the return. The larger
restored Cell and additional certificate also contribute to the
byte difference. These are transaction and receipt measurements
from the devnets. The initial shared run stopped because a network
port was occupied, and the verifier could not find its report. Both
failures are recorded with the successful runs on different ports.

The workspace passed 484 tests with 22 ignored. Regression devnets
verified CKB and xUDT round trips, capped Metatype UDT, Spore
settlement, four fee payouts, both L1 Cell Reference modes, and
funded Jolteon handoff with historical Intercell delivery. The Go
proof suite and CometBFT build check passed.

[Signed cases, proof traces, build fingerprints, measurements and commands](benchmarks/2026-11-26-stem-metatype-live-rule.json.xz).

## CKB reference-set certificates

The transaction now carries one certificate over one to four records
at the same CKB genesis and anchor. Each guardian checks the individual
proofs under a stable tip, then signs the complete OutPoint-ordered
set. The verifier checks one weighted quorum. Record count and bytes
still enter the logical read budget individually. The Type and Lock
Scripts receive the same individual records through syscall 2130.

These paired local devnets use four Jolteon replicas and the same
RISC-V contracts and workload on either side. The Metatype fixture
restores a 200-byte revision using a historical mailbox output and
the live rule registry. The Intercell fixture receives a 4,096-byte
message with three references. Bytes are the complete signed Stem
transaction and cycles come from the finalized execution receipt.

| Workload | References | Separate certificates bytes / cycles | Reference set bytes / cycles | Cycle reduction |
| --- | ---: | ---: | ---: | ---: |
| Metatype live rule return | 2 | 3,747 / 26,153,686 | 3,541 / 14,153,635 | 45.9% |
| Intercell receive | 3 | 19,337 / 37,925,251 | 18,925 / 13,925,148 | 63.3% |

Each quorum used three signatures at four million cycles per
signature. The two workloads save one and two quorum checks
respectively. Both passed again with the fixture's ordinary
20-million-cycle transaction limit. A rebuilt WASM SDK independently
verified the corresponding finalized receipts and the restored
three-Script Metatype Cell.

The controls rejected duplicate OutPoints, mixed sets from different
signers, a removed record under the original signatures, signature
tampering, an old anchor, and a historical rule substituted for a
live one. Reordering the requested OutPoints produced identical
guardian votes. Intercell covered destination binding, replay after
restart and the CKB reorganization case. The workspace passed 486
tests with 22 ignored. The CKB and xUDT bridge, paid security-domain
receipt, capped Metatype UDT, Spore, funded handoff, and individual
L1 Cell Reference regressions passed. Earlier build failures and the
security fixture's missing argument are in the archive.

[Signed transactions, measurements, proof traces and reproduction commands](benchmarks/2026-11-26-stem-reference-set.json.xz).

## Reusing verified CKB reference sets

The reference verifier keeps up to 64 successfully authenticated
sets per bridge configuration. Admission and candidate execution
can reuse signature checks. Intercell and StemVM share the decoded
records, and the VM reuses the typed Cell Data projection when
requested. The verifier checks the committed anchor and live age
on every use, and binds its authorization to the whole witnessed
transaction. Fixed signature cycles, logical reads and receipt
encoding are identical on hits and misses.

The verifier probe uses signed transactions from the preceding
Metatype and Intercell devnets and compares against `bfab628`.
Each sample runs 1,536 calls on one pinned logical CPU. The versions
run in baseline, candidate, candidate, baseline order, with five
samples per invocation. The figures are median calling-thread CPU
time per invocation of the verifier, including transaction
commitment hashing. Transaction decoding and database anchor lookup
are outside the timed path. The cold probe uses a fresh verifier
per call and constructs it outside the timer.

| Fixture | Warm baseline / candidate | Cold baseline / candidate | Warm CPU reduction |
| --- | ---: | ---: | ---: |
| Two references, Metatype return | 98.82 / 5.09 µs | 102.99 / 97.19 µs | 94.9% |
| Three references, Intercell receive | 131.67 / 25.56 µs | 126.72 / 135.58 µs | 80.6% |

The three-reference cold path took 7.0% more CPU time. These samples
measure repeated verification on fixed signed transactions. Network
cache hit rates and throughput are outside the measurement.

Separate paired runs on four Jolteon replicas and real CKB devnets
produced the same fees, cycles and signed transaction sizes in both
builds. The Metatype return used 14,153,635 cycles and 3,541 bytes.
The Intercell receive used 13,925,148 cycles and 18,925 bytes. The
WASM SDK independently checked all four finality and receipt
proofs and the two restored three-Script Metatype Cells. The
Intercell runs exercised restart, replay rejection and a CKB reorg.

Warm-cache controls rejected a changed anchor, a stale live
observation, altered signatures or records, use under a different
network or guardian set, and reuse of an authorization on a
different transaction. The workspace passed 486 tests with 22 ignored.
Six more devnets verified both CKB reference modes, the CKB and
xUDT bridge, four fee-fund payouts, capped Metatype UDT, Spore
settlement preserving the original ID, and the funded Jolteon
handoff with historical Intercell delivery. The Go proof suite and
CometBFT build check passed. The archive includes the initial
workspace and benchmark-helper build errors, an incompatible WASM
binding tool version, and a fee-run command that combined mutually
exclusive modes.

[CPU samples, signed inputs, execution proofs, build fingerprints and commands](benchmarks/2026-11-26-stem-reference-cache.json.xz).

## Append-only Metatype revisions

Four Jolteon replicas and a CKB devnet exercised an object with
identity, independent approval, a live length rule, and an append-only
Type Script. The native and portable groups use the same relation over
canonical revisioned data. Stem confirmed revision 1. CKB appended
content in revisions 2 and 4, and Stem restored the 200-byte state
at revision 4. The 120-byte rule rose to 200 during custody. WASM
verified finality, four execution stages per configuration, and
equal Capacity and full ordered Script sets for the escrowed and
restored Cells.

All four paired runs used the same valid content and revision sequence,
and locked 1,000 CKB in the CKB representation. The three-constraint
controls omit the append-only group. The CKB measurements are from the
second CKB revision. The Stem measurements are from the finalized return.

| Constraints and layout | Final CKB occupied | Shared definition occupied | CKB revision bytes / cycles | Raw witnesses | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: | ---: |
| Three, inline | 820 CKB | — | 1,741 / 4,447,868 | 170 B | 3,541 / 14,153,863 |
| Four, inline | 909 CKB | — | 1,867 / 4,797,035 | 170 B | 3,647 / 14,196,269 |
| Three, shared | 429 CKB | 491 CKB | 1,387 / 4,496,091 | 170 B | 3,541 / 14,153,863 |
| Four, shared | 431 CKB | 580 CKB | 1,426 / 4,868,977 | 170 B | 3,647 / 14,196,269 |

The fourth group added 7.9% and 8.3% CKB cycles in the inline and
shared deployments. The Stem return added 42,406 cycles.
Submission through local mining took 314–416 ms at this revision
across the four runs, including RPC and block production. The
definition is separately funded and can serve multiple Cells.

Both hosts rejected the same four proposed successor states given
the same input bytes and valid owner and approval authorization.
The cases rewrote or truncated the prefix, changed MIME, or bumped
the revision without appending. Both hosts rejected removal of the
constraint, and Stem rejected rewriting the restored object.

The first test invocations expected a specific Type Group to report
an invalid initial revision. The additional group could reject the
invalid transaction first. The corrected fixture accepts either of
the two applicable rejection reports for that case. A separate run
with 900 CKB failed CKB occupied-capacity validation in the inline
layout at 200 bytes. The shared layout completed at that funding
level. The archive includes all those runs.

487 workspace tests passed with 22 ignored. The real-network
regressions covered the dedicated two-Script object mapping, Spore
ID preservation, CKB and xUDT, capped Metatype UDT, four fee fund
payouts, Intercell recovery, and a funded committee handoff. The Go
proof suite and frozen CometBFT build check passed.

[Signed cases, proofs, cycle and capacity measurements, fingerprints, commands and failures](benchmarks/2026-11-26-stem-metatype-append-only.json.xz).

## Metatype workflow history

Four Jolteon replicas and a real CKB devnet confirmed an object with
identity, ownership approval and an independent signed workflow. The
request moved revision 0 to 1 on Stem. The CKB review and completion
were separate committed transactions at revisions 2 and 3. The Stem
return verified the two step signatures in order and restored the
120-byte state, the three original Type Scripts and 200 CKB.

| Manifest | CKB object occupied | Shared definition occupied | CKB completion bytes / cycles | CKB raw witnesses | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: | ---: |
| Inline | 867 CKB | — | 1,881 / 6,759,827 | 329 B | 3,222 / 18,395,115 |
| Shared | 349 CKB | 618 CKB | 1,400 / 6,850,759 | 329 B | 3,222 / 18,423,397 |

Both releases used the existing 20-million-cycle Stem transaction
budget. Each returned transaction carries a 128-byte group proof.
Local submission through mining for the CKB review and completion
ranged from 423 to 481 ms over the two layouts, including RPC and
block production. The CKB object locked 1,000 CKB in both runs.

CKB rejected presenting two valid step signatures for one edit,
replaying a prior step, a foreign network domain and a missing
signature. Stem rejected premature export and refused a return
with missing, incomplete, reordered or altered proofs, even with an
authenticated CKB burn. An offline check replayed all three Ed25519
signatures, verified CKB transaction and witness inclusion for the
actual import, review, completion and burn, and checked that each
spend consumed its predecessor OutPoint. WASM independently verified
Stem finality, execution and restored Cell proofs.

488 workspace tests passed with 22 ignored. The Go proof suite,
CometBFT comparison build and six real-network regression runs passed.
They exercised append-only custody, CKB and xUDT, Spore, Intercell,
funded committee handoff and four fee payouts. The first CKB build
reported a missing Molecule trait import, which the archived rebuild
corrected.

[Signed transactions, inclusion and finality proofs, measurements, build fingerprints and commands](benchmarks/2026-11-26-stem-metatype-workflow.json.xz).

## Recovering Metatype workflow proofs from CKB

The recovery CLI starts with an authenticated Stem escrow Cell,
a trusted CKB anchor, and the CKB import and burn OutPoints. It follows
the actual CKB spends, verifies transaction and witness inclusion under
the anchor, and replays the workflow signatures. The lab used a real
CKB devnet and four Jolteon replicas in each configuration. Each
CKB path included an ownership transfer with identical workflow data.

| Return case | CKB object Cells on path | Recovered proof | First / fresh process | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: |
| Inline, revision 3 | 4 | 128 B | 21.0 / 25.2 ms | 3,222 / 18,430,390 |
| Shared, revision 3 | 4 | 128 B | 37.9 / 41.1 ms | 3,222 / 18,407,719 |
| Shared, revision 2 | 3 | 64 B | 27.5 / 35.1 ms | 3,158 / 16,298,870 |

The timings are individual loopback samples from CLI process launch
through the verified JSON result. Each pair used two fresh processes
with the same public request and produced identical output. The real
Stem return used the recovered bytes in its group witness. WASM
verified the resulting Block, execution and restored Cell proofs.

The CLI rejected a stale anchor, a wrong import, and a wrong burn
output. After the early return, the fixture replaced the CKB block
containing the burn. The old anchor was unavailable and recovery
refused it. The raw record retains the original and replacement hashes.
The first runs found a difference between native Script ordering and
the required Molecule Manifest order. The archived retry sorts the
original Scripts by their Molecule bytes. Both initial Rust build
failures are recorded.

The workspace, Go proof suite and CometBFT comparison build passed.
Devnet regressions verified Intercell, CKB and xUDT, Spore, fee payouts,
funded committee handoff and append-only custody.

[Recovery requests, signed transactions, proofs, timings, commands and failures](benchmarks/2026-11-26-stem-workflow-recovery.json.xz).

## Atomic settlement of two Metatype assets

Four Jolteon replicas and a real CKB devnet settled two capped
assets with distinct series IDs, Manifest IDs, escrows
and mailboxes. The fixture exported 160 and 250 units, imported
both on CKB, exchanged their owners in one CKB transaction, burned
both, and released the native assets to distinct recipients. Each
stage used one transaction. The Stem return used a single
guardian reference-set certificate for both CKB mailbox outputs.

| Manifest layout | CKB import bytes / cycles | CKB swap bytes / cycles | CKB burn bytes / cycles | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: |
| Inline | 4,537 / 8,359,930 | 1,768 / 4,765,656 | 2,804 / 6,621,611 | 4,295 / 14,284,682 |
| Shared | 4,135 / 8,483,494 | 1,366 / 4,842,060 | 2,878 / 6,713,677 | 4,295 / 14,284,682 |

Each CKB asset occupied 481 CKB inline or 243 CKB with a shared
definition. Each shared definition occupied another 336 CKB.
The Stem export consumed 2,297,985 cycles and its receipt
committed both message IDs. The return ran under the existing
20-million-cycle transaction cap. The independent four-action
case used 7,683,284 cycles with a 128-byte result.

The CKB controls substituted one import witness, inflated one
swap output and misstated the second burn amount. All affected CKB
inputs remained live after rejection. Stem rejected an omitted
export action, a second action in the same Lock group, an omitted
burn reference, a redirected release and a replay. Before the
successful redeem, both escrow Cells remained live and both Inbox
records were absent. The five-action probe left every record absent.
Its successful four-action counterpart finalized exactly four
records and four ordered IDs.

WASM verified finality, execution receipts, message membership
and absence, and the original Type Scripts' presence on the
released Cells. Twelve CKB output inclusion checks verified both
outputs from each import, swap and burn under the recorded
headers with transaction and witness paths. The workspace passed
488 tests with 22 ignored. The Go proof suite and CometBFT build
check passed. Real CKB devnet regressions covered the single-lane
Metatype return, workflow proof recovery, Intercell, native CKB
and xUDT, Spore, fee payouts and funded committee handoff.

[Signed transactions, proofs, measurements, commands and rejection controls](benchmarks/2026-11-26-stem-metatype-multiasset.json.xz).

## Composed Metatype asset Cells

Four Jolteon replicas and a CKB devnet executed the same 160-unit and
250-unit capped assets in one Cell. The Stem fixture issued the assets
in distinct Cells, composed a Cell with four Type Scripts, exported it,
and restored that Script set on return. A final transaction split the
restored Cell into two scalar asset Cells with their original Scripts.
The CKB path imported one Metatype representation, transferred it
to another owner and burned it through the authenticated mailbox.

| Manifest layout and return | CKB import bytes / cycles | CKB transfer bytes / cycles | CKB burn bytes / cycles | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: |
| Inline, full | 2,738 / 5,621,512 | 1,335 / 3,126,918 | 1,767 / 4,795,284 | 2,908 / 14,257,163 |
| Shared, full | 2,369 / 5,715,803 | 864 / 3,189,930 | 1,804 / 4,875,064 | 2,908 / 14,257,163 |
| Shared, partial | 2,471 / 5,760,304 | 864 / 3,202,268 | 2,026 / 5,162,407 | 3,477 / 14,340,528 |

The inline representation occupied 771 CKB. The shared layout
occupied 263 CKB per representation and 610 CKB once for the
shared definition. Import witnesses totaled 723, 825 and 927
bytes across the three runs. Transfer and burn witnesses totaled 85
and 170 bytes respectively in each run. The Stem composition,
export and split consumed 2,026,304, 2,265,902 and 2,026,295 cycles.
Every Stem return ran under the 20-million-cycle limit.

The partial return released 60 and 110 units. Stem retained 100
and 140 units in escrow, matching the two amounts in the surviving
CKB representation. Negative controls rejected noncanonical native
amount order, an exceeded independent cap, exchanging amounts
between asset keys, inflation, an unchanged component during partial
burn, misstated burn amounts, and substituted import witness data.
Admission also rejected missing message authorization or a missing
burn reference, misdirected release and replay. The separate four-
and five-action controls verified atomic admission at the action limit.

Against the earlier two-Cell shared fixture, full shared return fell
from 4,295 to 2,908 Stem bytes. Its CKB transfer fell from 1,366 to
864 bytes and from 4,842,060 to 3,189,930 cycles. The two-Cell
fixture settled assets to distinct owners. The composed fixture moved
both under a common Lock and stored two quantities in a single
representation. These are individual transaction measurements.

Independent WASM verification checked Jolteon finality, execution
receipts, message consumption and exact Cell state, including both
residual escrow quantities. Twelve offline CKB checks authenticated
the signed transaction and Witness paths for the recorded outputs.
The Rust workspace passed 489 tests with 22 ignored. The Go proof
suite and CometBFT comparison build passed. Eight further devnet
runs covered separate two-asset settlement, single-asset Metatype,
workflow recovery, Intercell, CKB and xUDT, Spore, fee payouts
and a CKB-funded committee handoff. The 2–8-key encoding is
bounded in code. The composed custody devnet fixture used two
assets and four Type Scripts.

[Signed data, proofs, build fingerprints, commands and rejection controls](benchmarks/2026-11-26-stem-composed-assets.json.xz).

## Selective Metatype redemptions

Four colocated Jolteon replicas and a real CKB devnet ran two selective
returns and repeated the previous balanced and full return fixtures.
Both assets entered custody with 160 and 250 units. The inline selective
return moved 60 units of the first asset to Stem and retained 100 and
250 units. The shared selective return moved all 160 units of the first
asset, leaving 0 and 250. A second burn then returned all 250 units,
with the same Manifest and the next Mailbox nonce. The final Stem
escrow held only its Capacity bond.

| Layout and return | Asset deltas | CKB burn bytes / cycles | Stem redeem bytes / cycles |
| --- | ---: | ---: | ---: |
| Inline selective | 60, 0 | 2,497 / 5,072,630 | 3,477 / 14,342,599 |
| Shared selective, first | 160, 0 | 2,026 / 5,139,674 | 3,477 / 14,342,664 |
| Shared selective, remainder | 0, 250 | 1,804 / 4,889,003 | 2,908 / 14,258,776 |
| Shared balanced | 60, 110 | 2,026 / 5,134,543 | 3,477 / 14,342,599 |
| Shared full | 160, 250 | 1,804 / 4,931,895 | 2,908 / 14,258,620 |

Every CKB burn carried 170 witness bytes. All Stem returns stayed
under the existing 20-million-cycle transaction limit. The shared
balanced and full fixtures retained the previous transaction sizes.
These are per-transaction measurements under local test conditions.

CKB rejected a burn that increased one asset while decreasing the
other, a declared delta inconsistent with the burn, and a proposed live representation
with two zero amounts. Stem rejected a release that preserved total
supply yet took an extra unit of the asset excluded from the CKB
message. The original cap, canonical ordering, signed reference and
replay controls also passed. The asset and cap groups ran for the
zero slot. CKB kept the complete Manifest, and Stem kept all four
original Type Scripts on the asset Cells.

Independent WASM verification checked the four Stem runs and both
redemptions in the sequential case. Twenty-one offline CKB proof
checks authenticated the transactions and witnesses, including the
shared Manifest definitions. The verifier decoded the quantities
of each authenticated residual output by full asset Script hash.
Ten devnet regressions included standalone inline and shared 2, 4,
8 and 16 constraint deployments, dual-lane settlement, scalar UDT,
workflow recovery, Intercell, CKB/xUDT, Spore, fee payouts and funded
committee handoff. The Rust workspace passed 490 tests with 22
ignored. The Go proof suite and CometBFT build check passed.

The evidence includes the initial Rust and RISC-V build failures
from a missing import and the first offline checker failure from
reading the transaction hash at the wrong JSON level. Corrected
reruns succeeded.

[Signed transactions, proofs, result JSON, commands and failed attempts](benchmarks/2026-11-26-stem-selective-metatype.json.xz).

## Metatype object royalties

Four Jolteon replicas and a real CKB devnet tested a three-constraint
object with identity, approval and a fixed 110 CKB royalty. Three
round trips covered immutable objects with inline and shared Manifests
and a revisable object with an inline Manifest. Each run paid on a
native holder change, export into escrow, transfer on CKB and release
on Stem. The revisable run also committed same-Lock updates on both
chains without a royalty payment.

| Object and layout | CKB representation occupied | CKB transfer bytes / cycles | CKB import cycles | CKB burn cycles |
| --- | ---: | ---: | ---: | ---: |
| Immutable inline control | 563 CKB | 1,373 / 4,060,865 | 5,026,855 | 4,163,551 |
| Immutable inline royalty | 724 CKB | 1,712 / 4,463,189 | 5,388,474 | 4,527,721 |
| Immutable shared control | 293 CKB | 1,140 / 4,141,556 | 5,079,482 | 4,250,472 |
| Immutable shared royalty | 295 CKB | 1,320 / 4,528,015 | 5,496,061 | 4,585,308 |
| Revisable inline royalty | 729 CKB | 1,737 / 4,519,449 | 5,383,544 | 4,515,496 |

The shared immutable definition occupied 529 CKB with the royalty
and 368 CKB in the control. Every transfer in the table carried
170 raw witness bytes. The paired CKB transfer cost rose 9.9%
inline and 9.3% shared, including the extra adapter and payment
output. Across the three royalty runs, Stem export used
4.103–4.105 million cycles, native transfer used 3.924 million and
release used 14.176 million. The revisable same-Lock Stem update
used 3.910 million cycles. These measurements describe individual
transactions on a colocated development network.

Stem and CKB rejected missing, underpaid, misdirected, wrongly
scoped and duplicate payments. Both rejected removal of the
royalty rule. Stem separately rejected missing payments at the
export and release boundaries. Independent WASM verification
checked three finalized Stem payment Cells per run, and verified
membership of all original Type Scripts on the escrowed and
restored objects. Twelve CKB offline checks verified transfer
payments, object output inclusion and mailbox outputs against
their recorded transaction and witness roots.

Eight further devnet runs covered workflow recovery, fungible
Metatype, selective composed-asset redemption, Intercell,
CKB/xUDT, Spore, fee payouts and funded committee handoff.
The Rust workspace passed 491 tests with 22 ignored. The Go proof
suite and the CometBFT build check passed.

[Signed payments, paired controls, rejection logs, proofs and reproduction commands](benchmarks/2026-11-26-stem-metatype-royalty.json.xz).

## Metatype approval key rotation

Four Jolteon replicas and a real CKB devnet used the same object
and approval Scripts across a change of Ed25519 key. Stem issued
and exported with generation 0, and CKB imported with that key.
The CKB Type ID registry then advanced to generation 1. CKB
transferred and burned with the new key, and Stem accepted the
return after submitting a confirmed anchor and fresh registry
observation. Inline and shared fixtures used three Type Groups.
The revisable shared fixture added the independent live ceiling
group, increased its ceiling from 120 to 200 bytes on CKB, and
returned using one certificate with both live records and the
historical mailbox output.

| Fixture | CKB key rotation bytes / cycles | CKB transfer bytes / cycles | Stem export bytes / cycles | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: |
| Inline, three groups | 772 / 1,793,740 | 1,772 / 6,683,822 | 2,609 / 18,203,027 | 3,246 / 16,277,231 |
| Shared, three groups | 809 / 1,766,305 | 1,420 / 6,732,323 | 2,609 / 18,221,543 | 3,246 / 16,292,938 |
| Shared, rotating key and ceiling | 920 / 1,794,368 | 1,557 / 7,146,679 | 3,098 / 18,278,278 | 4,129 / 16,361,918 |

The CKB transfers each used 329 raw witness bytes. The
representations occupied 684, 295 and 301 CKB respectively.
The table measures individual transactions. All Stem operations
fit the existing 20-million-cycle limit.

CKB rejected changing the registry owner, repeating the key,
skipping a generation, supplying an invalid key and retiring the
registry. CKB also rejected the retired signer on transfer and
burn and could no longer resolve the spent dependency. Stem
rejected the previous signer against the current registry,
a return with only historical registry data and a missing seal.
The combined case also rejected omitting the ceiling while still
supplying the correct current approval key and mailbox.

Independent WASM checks authenticated finality, execution receipts
and the restored Type Scripts in all three cases. Fifteen offline
CKB inclusion checks covered both key records and the import,
transfer and burn. Separate Ed25519 verification checked the
recorded exact-transaction seals on both chains. Ten additional
devnet runs covered royalties, workflow recovery, fungible and
selective Metatype returns, live ceilings, Intercell, CKB/xUDT,
Spore, fee payouts and a funded committee handoff. The Rust
workspace passed 492 tests with 22 ignored. The Go proof tests
and CometBFT build check passed.

The archive includes the initial build and fixture failures.
The combined runner originally used the wrong reserve position
and then selected the wrong registry code dependency. It also
needed a key-only attestation to isolate the missing-ceiling
control. A separate fungible rerun moved to fresh ports after a
CKB RPC bind conflict.

[Signed registry changes, detached seals, proofs, commands and failures](benchmarks/2026-11-26-stem-metatype-rotation.json.xz).

## Metatype freeze and thaw

Four Jolteon replicas and a real CKB devnet exercised two freezes
without changing the object, approval Script or Type ID. The inline
fixture used three Type Groups. The shared revisable fixture also
used an independently authenticated live ceiling. In both runs,
the key rotated from generation 0 to 1. The registry then froze at
generation 2, thawed for the CKB burn at generation 3, froze again
while the return was pending at generation 4, and thawed for Stem
release at generation 5. All four freeze and thaw updates retained
the same public key.

| Fixture | CKB freeze or thaw bytes / cycles range | CKB burn bytes / cycles | Stem release bytes / cycles |
| --- | ---: | ---: | ---: |
| Inline, three groups | 773 / 1,785,378–1,816,968 | 2,014 / 6,731,808 | 3,247 / 16,272,880 |
| Shared, approval and live ceiling | 921 / 1,793,231–1,807,787 | 2,296 / 7,186,780 | 4,130 / 16,356,241 |

CKB rejected transfers and burns signed against a frozen registry.
Stem rejected release of the already burned object once the frozen
record reached its committed anchor. Its trusted state showed a
live escrow and an absent Inbox record. The next anchored thaw
allowed release of the exact same message. Both hosts also rejected
a seal from an earlier generation of the same public key. These
measurements cover individual local transactions.

Independent verification checked 18 CKB output inclusions, the
accepted transaction seals and the valid Ed25519 seals on each
rejected frozen operation. WASM verified the two finalized
Inbox absence proofs during the freeze, later membership of the
same messages, live escrow before thaw and restored Type Groups
afterward. The 11 devnet regressions covered ordinary key rotation,
live ceilings, royalties, workflow, single and composed UDTs,
Intercell, CKB/xUDT, Spore, fee payouts and funded handoff.
The Rust workspace passed 493 tests with 22 ignored. The Go proof
suite and the CometBFT build check passed.

The first two devnet runs stopped because the harness tried the
generic missing-approval control while the freeze was still active.
The reruns isolated each check. The archive also retains the first
offline checker failure, which passed the new state byte to the
public-key decoder.

[Registry changes, signed tests, pending and released proofs, measurements and failed runs](benchmarks/2026-11-26-stem-metatype-freeze.json.xz).

## Metatype quorum approval

Five rounds on four Jolteon replicas and a real CKB devnet
issued, exported, transferred, burned and recovered the same
object within each round. The inline pair used the same immutable
profile for single- and double-signature approval. The shared
Manifest rounds used a revisable object with a live CKB size rule.
Each round rotated the roster while the object was on CKB and
froze the object before burn and again before the pending Stem
release. All five returned the original constraints to Stem and
consumed the same pending Inbox message after thaw.

| Manifest and quorum | CKB import bytes / cycles | CKB transfer bytes / cycles | CKB burn bytes / cycles | Stem export bytes / cycles | Stem release bytes / cycles |
| --- | ---: | ---: | ---: | ---: | ---: |
| Inline, 1 of 1 | 3,205 / 7,605,250 | 1,773 / 6,700,042 | 2,015 / 6,777,867 | 2,612 / 18,217,493 | 3,249 / 16,297,491 |
| Inline, 2 of 3 | 3,167 / 9,746,034 | 1,837 / 8,814,415 | 2,079 / 8,866,662 | 2,740 / 20,337,838 | 3,377 / 18,413,801 |
| Shared, 2 of 3 | 2,936 / 10,290,004 | 1,622 / 9,357,049 | 2,361 / 9,444,274 | 3,261 / 20,363,224 | 4,292 / 18,456,744 |
| Shared, 3 of 5 | 3,102 / 12,443,734 | 1,686 / 11,528,899 | 2,425 / 11,555,685 | 3,389 / 22,490,757 | 4,420 / 20,521,177 |
| Shared, 8 of 8 | 3,320 / 23,160,153 | 2,006 / 22,299,818 | 2,745 / 22,342,342 | 3,805 / 32,960,279 | 4,836 / 31,050,991 |

CKB import sizes include the variable Jolteon checkpoint witness.
The quorum proof itself occupied 65, 129, 193 and 513 bytes for
one, two, three and eight signatures. CKB approval registry data
occupied 42, 106, 170 and 266 bytes for the corresponding rosters.
The mapped object occupied 684 CKB in both inline rounds and
301 CKB in the shared revisable profile. The paired inline release
added 2,116,310 cycles and 128 transaction bytes for two signers.
The increment consists of one 64-byte signature in the group
Proof Data and two additional 32-byte keys in the authenticated
L1 registry record.
See the raw receipts and witnesses for the individual sizes.

The first 20-million-cycle development runs stopped at native
validation, and the explicit default-limit rerun reproduced the
cycle-exhaustion result. The 1-of-1, 2-of-3 and 3-of-5 comparison
used the same 28-million-cycle fixture limit. The 8-of-8 round
used 40 million cycles. The fixture's default remains 20 million.
The cycle bounds and transaction fees applied to every committed
transaction. These runs measured individual operations, with raw
CKB submit-through-local-mining timings in the archive.

Both hosts rejected underfilled, duplicated, out-of-order and
foreign-network signatures for applicable roster sizes. They
rejected an out-of-roster bitmap when the roster used fewer than
eight positions. The CKB registry rejected malformed keys,
thresholds, generations and unauthorized controller changes.
Stem rejected historical inclusion as a substitute for a live
record. CKB rejected a consumed registry Cell Dep. The offline
checker independently verified the selected signatures, including
valid quorums that the two chains rejected while frozen, and 45
CKB transaction and witness inclusion proofs. The WASM verifier
checked Jolteon finality, executions, continued escrow and the Inbox
proofs before and after release. The 2-of-3 runs accepted all
three signer subsets across the verified successful operations.

The release workspace passed 494 tests. The Go proof suite and
the frozen CometBFT build check also passed. The CKB and Stem
network regressions covered workflow proofs, selective multiasset
returns, Intercell, xUDT, fee payouts, funded committee handoff,
Spore Cluster preservation, royalties and ceiling-proof recovery.

Reproduce with the rotating approval fixture in [OPERATIONS.md](OPERATIONS.md).
The archive contains exact commands, build fingerprints, signed
transactions, fixtures, verification scripts and unsuccessful
attempts. [Quorum experimental records](benchmarks/2026-11-26-stem-metatype-quorum.json.xz).

## Metatype ceiling decreases

Four Jolteon replicas and a real CKB devnet ran two revisable objects
through an adjustable ceiling. The inline fixture had three Type
Groups. The shared fixture also had a rotating approval and froze the
pending return before thawing. In both fixtures a 70-byte state
committed on Stem was imported after lowering the CKB ceiling to
69 bytes. CKB later admitted a 200-byte revision under a 200-byte
ceiling, kept those exact bytes through an owner transfer under a
120-byte ceiling, and burned the representation. A second decrease
before Stem release set the current ceiling to 100 bytes. Stem
restored the exact 200-byte state with the pinned custody proof and
then committed a 68-byte revision under the lower ceiling.

| Fixture | CKB import bytes / cycles | CKB unchanged transfer bytes / cycles | CKB burn bytes / cycles | Stem return bytes / cycles | Return route proof |
| --- | ---: | ---: | ---: | ---: | ---: |
| Inline, three groups | 3,342 / 5,694,741 | 1,773 / 4,517,465 | 1,981 / 4,542,801 | 4,246 / 14,228,091 | 666 B |
| Shared, rotating approval and freeze | 3,103 / 8,353,864 | 1,667 / 7,264,839 | 2,296 / 7,284,497 | 4,900 / 16,412,036 | 731 B |

CKB import witnesses totaled 1,257 and 1,291 bytes respectively.
A compliant Stem edit after release consumed 15,916,954 and
18,066,317 cycles. All Stem operations fit the existing 20-million-
cycle transaction limit. These are individual transaction measurements.

CKB rejected oversized imports with missing or substituted route
proofs and rejected editing a still-oversized state even at the
same length. Stem rejected missing return evidence and, in the
inline case, a substituted wrapper or adapter table. Both fixtures
also rejected applying the exact return proof to an ordinary
oversized edit. The combined fixture verified that the frozen and
thawed Inbox states preserve the same return message.

Independent verification checked 19 CKB output inclusions including
both sets of registry updates. It decoded the exact imported,
preserved and burned bytes and checked the returned deployment and
adapter table commitment. WASM independently verified both Stem
returns, all original Type Groups and the final 68-byte state.
Eleven further devnet runs covered live-ceiling growth, key
rotation, royalties, workflow recovery, scalar and composed UDTs,
Intercell, CKB/xUDT, Spore, fee payouts and funded handoff. The Rust
workspace passed 493 tests with 22 ignored. Go proof tests and the
CometBFT comparison build check passed.

The archive retains four failed fixture attempts. Two negative
controls omitted the old dependency after a registry update instead
of removing the active dependency. The shared fixture also exceeded
the CKB pool's transaction size limit when publishing all code
together. The final runner resolves the actual registry head and
splits the deployment into two transactions under the same pool
limits.

[Registry updates, signed transfers, route proofs, finality and failed runs](benchmarks/2026-11-26-stem-metatype-ceiling.json.xz).

## Recovering Metatype ceiling proofs from CKB

Four Jolteon replicas and a real CKB devnet used
`stem ckb-wallet recover-metatype-ceiling` to build the proof for
each oversized return. The CLI authenticated the burn and the
consumed Metatype output against the supplied CKB anchor. It
verified the mailbox record and recovered the physical Script,
actual adapter table and ceiling dependency from CKB. The returned
proof matched the independently constructed bytes and was used to
sign and finalize the Stem return.

| Fixture | Proof size | First recovery | After restarting CKB | Stem return bytes / cycles |
| --- | ---: | ---: | ---: | ---: |
| Inline | 666 B | 29.1 ms | 31.9 ms | 4,246 / 14,228,091 |
| Shared with rotating approval and freeze | 731 B | 31.7 ms | 36.9 ms | 4,900 / 16,415,275 |

The times are individual local CLI calls, including process start
and CKB RPC. Each real CKB restart yielded identical proof bytes.
In the shared case the first call used the frozen anchor and the
second used the thawed anchor. The same 200-byte state and mailbox
record survived both. The CLI recovered the 120-byte ceiling used
by the burn. Stem used the later live ceiling of 100 bytes and
accepted a subsequent 68-byte revision.

The CLI rejected an anchor preceding the burn, a different output
in the actual burn transaction, an unrelated Stem network, and an
absent ceiling Script. An actual CKB reorg removed the inline
fixture's previously trusted anchor. Recovery rejected the orphaned
branch even though the Stem release had already finalized.

Independent verification checked 19 CKB output inclusions,
recovered route preimages and the exact object and mailbox bytes.
WASM checked both releases, all original Type Groups, the frozen
Inbox state and the final 68-byte revisions. Eleven further devnet
runs covered the shared workflow recovery implementation, live
ceiling increases, key rotation, royalties, fungible and selective
Metatype, Intercell, CKB/xUDT, Spore, fee payouts and funded
handoff. The Rust workspace passed 493 tests with 22 ignored.
Go proof tests and the CometBFT comparison build check passed.

[Recovery requests and results, CKB restarts and reorg, signed transactions and proof checks](benchmarks/2026-11-26-stem-metatype-ceiling-recovery.json.xz).

## On-demand VM code dependency indexing

Baseline `5c8f8bf` and candidate `4a84463` used release builds with
Rust 1.95.0 on one i9-13900H. Cell and dependency resolution still
validates live references and commits their contents before checking
the VM cache. A single expanded Code dependency uses direct lookup.
Several Code dependencies share an index initialized on first Script
lookup. Cache hits retain their recorded cycles and need no index.

The pinned-core resolver probe alternated binaries over seven rounds
of 200,000 resolutions per configuration. Both binaries used the same
Cell records. The single-dependency fixture was a signed payment and
its actual RISC-V Lock dependency from the network run. The synthetic
eight-dependency fixture adds seven typed Code Cells. The probe computes
the VM cache key in every case and optionally resolves each program.
It does not execute the RISC-V programs or include database reads.

| Resolver work | Baseline median | Candidate median | Reduction |
| --- | ---: | ---: | ---: |
| One Code dependency, through cache key | 4.91 µs | 4.79 µs | 2.5% |
| One Code dependency, including lookup | 5.02 µs | 4.78 µs | 4.7% |
| Eight Code dependencies, through cache key | 11.84 µs | 8.04 µs | 32.1% |
| Eight Code dependencies, including lookups | 12.76 µs | 12.41 µs | 2.7% |

The payment experiment used four Jolteon replicas, 250 ms proposal
cadence, a 2 billion cycle block limit, two VM workers per replica and
64 client senders. Each rate used the same signed transaction bytes,
genesis and test executable for both revisions. The 20-second windows
below are the paired reruns. Every offered transaction became visible
on all four nodes during the run or its 12-second drain.

| Planned TPS | Build | Offered TPS | Admitted TPS | All-replica visible TPS | With drain TPS | p50 / p95 / p99 | Client drops |
| ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1,000 | Baseline | 989.30 | 989.30 | 956.55 | 965.47 | 0.788 / 1.253 / 1.365 s | 214 |
| 1,000 | Candidate | 992.90 | 992.90 | 959.75 | 973.06 | 0.747 / 1.100 / 1.315 s | 142 |
| 2,000 | Baseline | 1,647.10 | 1,647.05 | 1,568.85 | 1,607.83 | 1.042 / 1.370 / 1.515 s | 7,058 |
| 2,000 | Candidate | 1,594.95 | 1,594.90 | 1,520.15 | 1,550.11 | 1.067 / 1.431 / 1.551 s | 8,101 |

At the 1,000 TPS target the replicas consumed 44.44 and 43.51 CPU seconds,
wrote 255.65 and 269.61 MB at the process level, and moved 60.37 and
62.58 MB over the network, baseline first. Maximum sampled per-replica
RSS was 124.95 and 124.31 MiB. The first 1,000 TPS candidate run coincided
with a separate host process using roughly 11 logical cores and offered
only 460.40 TPS. The archive retains those runs and host pressure samples.
The payment pairs show no consistent end-to-end throughput improvement.

The same four-replica setup offered all 2,880 signed distributed AMM
commands at 160 TPS in an 18-second comparison. Both builds finalized
every command. All-replica visible throughput was 152.94 and 153.83 TPS,
with p99 of 0.855 and 0.967 seconds. Serial replay verified the receipts
and 5,760 Lineage head updates in each run. The 65 final Lineage IDs,
versions and canonical Cell contents agreed across builds. Each build
charged 11,295,925,110 total VM cycles and 5,760,000 shannons.

A mixed four-replica network finalized all 96 signed payments with one
replica offline and later verified a common Header and two-chain proof
after restart. Real CKB devnet runs covered fee payouts, xUDT and Spore
round trips, funded committee handoff, Intercell messaging and shared
Metatype ceiling recovery. The archive includes commands, signed inputs,
proofs, raw measurements and failed setup attempts.

`cargo test --locked --release --workspace` passed 494 tests with 22
ignored after recompiling the vendored CKB test harness at the new
checkout path. The initial shared-build run used the previous absolute
fixture path and its failures are retained. `go -C tools/proof test
-count=1 ./...` and the release `comet-benchmark` feature check passed.

[Resolver samples, four-replica measurements and CKB devnet evidence](benchmarks/2026-11-26-stem-program-resolution.json.xz).

## Shared block context and resolved transaction IDs

Baseline `83402fa` and candidate `379bc89` used the same release profile,
Rust 1.95.0, and identical RISC-V Lock code on the i9-13900H host.
The candidate derives each execution context from the exact immutable
parent and network once. Lock authorization, VM execution and output
OutPoints share the transaction ID established during resolution.

The pinned-core probe ran each binary seven times at 150,000 repetitions
for each configuration. It took a signed payment and its funded input
and Code dependency from the baseline network run. Each repetition
resolves the transaction, obtains its output transaction ID and
computes its VM cache key. Batch repetitions share one block context.
Admission repetitions derive a new context for each transaction. The
probe reads in-memory Cells and stops before RISC-V verification. All
observations agreed on the transaction ID, parent ID, cache key, fee and
Cell read count.

| Resolver work | Baseline median | Candidate median | Reduction |
| --- | ---: | ---: | ---: |
| Per-transaction resolution in a shared candidate | 5.34 µs | 4.41 µs | 17.4% |
| Resolution including a fresh admission context | 5.92 µs | 4.93 µs | 16.7% |

The four-replica payment runs fixed 250 ms proposal cadence, a 2 billion
cycle block limit, two VM workers per replica and 64 senders. Each
comparison used identical genesis and signed inputs. At a planned
2,000 TPS over 20 seconds with fresh connections, the adjacent reruns
produced these results. The first baseline reached only 933.55 offered
TPS and is retained in the archive with the other raw measurements.

| Build | Offered TPS | Admitted TPS | All-replica visible TPS | With drain TPS | p50 / p95 / p99 | Client drops |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| Baseline | 1,583.80 | 1,582.45 | 1,494.65 | 1,521.80 | 1.106 / 1.481 / 1.588 s | 8,324 |
| Candidate | 1,589.70 | 1,589.25 | 1,506.95 | 1,528.12 | 1.085 / 1.426 / 1.483 s | 8,206 |

Two 60-second comparisons used persistent submission at a planned
1,000 TPS. The first ran the candidate first. The second reversed
that order. All offered payments finalized on all replicas.

| Pair | Build | Offered / admitted TPS | All-replica visible / with drain TPS | Visible | p50 / p95 / p99 | Replica CPU seconds | Process writes | Network traffic |
| --- | --- | --- | --- | ---: | --- | ---: | ---: | ---: |
| First | Baseline | 1,000.00 / 999.98 | 985.88 / 991.55 | 60,000 | 0.739 / 0.962 / 1.060 s | 110.16 | 1,136.17 MB | 186.28 MB |
| First | Candidate | 1,000.00 / 1,000.00 | 985.47 / 990.93 | 60,000 | 0.818 / 1.058 / 1.116 s | 129.67 | 1,109.98 MB | 209.23 MB |
| Reversed | Baseline | 999.08 / 999.08 | 985.52 / 991.05 | 59,945 | 0.787 / 1.032 / 1.197 s | 121.21 | 1,135.03 MB | 198.22 MB |
| Reversed | Candidate | 999.00 / 999.00 | 987.07 / 991.62 | 59,940 | 0.824 / 1.104 / 1.261 s | 130.52 | 1,144.12 MB | 207.05 MB |

In the two unpinned sustained pairs, candidate CPU time was 17.7%
and 7.7% higher. A separate 24-second test bound each replica to the
same two logical siblings of a performance core and one efficiency
core on both versions. The client used disjoint CPUs and persistent
connections. The two pairs ran in opposite orders.

| Pair | Build | Offered / admitted TPS | All-replica visible TPS | Visible | p99 | Replica CPU seconds |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| First | Baseline | 1,000.00 / 999.96 | 965.96 | 24,000 | 1.125 s | 57.18 |
| First | Candidate | 987.00 / 986.96 | 959.62 | 23,688 | 1.534 s | 55.41 |
| Reversed | Baseline | 1,000.00 / 1,000.00 | 963.62 | 24,000 | 0.876 s | 37.74 |
| Reversed | Candidate | 1,000.00 / 1,000.00 | 970.83 | 24,000 | 0.995 s | 38.17 |

The pinned changes in CPU time per finalized payment were −1.8% and
+1.1%. Candidate p99 remained higher in those two payment runs. The
benchmarks establish the cheaper resolver path, with mixed throughput
and tail latency at the network level. The affinity launcher looked
for a validator key when the harness invoked its separate history index
command. All four network reports had already been saved. Re-running
that command directly against each finished archive generated all four
history proofs. The archive retains the original failing exits.

The distributed AMM test offered all 2,880 signed commands at 160 TPS.
Both builds finalized the full workload. The replay checked 5,760
Lineage head updates and identical final canonical Cells for 65 Lineages.
Each build charged 11,295,925,110 VM cycles and 5,760,000 shannons.
All-replica visible throughput was 151.39 and 152.44 TPS and p99 was
1.260 and 0.998 seconds, baseline first.

A mixed-version four-replica network finalized all 96 signed payments
and agreed on the execution Header after a replica restarted. The
four-replica composition fixture exercised Polytype, OTX, Lineage,
height and time triggers, and a prepaid Keeper retry. A separate mixed
load admitted all 150 Keeper bookings and verified 188 attempts, of
which 112 applied. CKB devnet regressions passed for fee payouts, xUDT,
funded committee handoff, Intercell, shared Metatype return and the
Spore V2 Cluster route.

The release workspace suite passed 494 tests with 22 ignored. The Go
proof suite and frozen `comet-benchmark` build check passed.

[Signed workloads, phase metrics, history proofs and build fingerprints](benchmarks/2026-11-26-stem-block-context.json.xz).

## Canonical execution records and Merkle allocations

Baseline `8e619ec` and candidate `4177d53` used the same
Rust release profile and i9-13900H host. The candidate builds the
receipt commitment from the same byte sequence sent to the write
batch. Transaction and receipt Merkle trees keep their internal
hashes as 32-byte values. The execution record moves into the batch
without a separate buffer copy.

The fixed-input probe used signed payments and real signed Metatype
transactions and receipts from earlier devnets. It formed blocks of
0, 1, 3, 31, 64, 256 and 1,024 entries with ordinary and mixed
records. The two builds produced byte-identical roots, stored
execution records and ordered execution proofs. The probe ran the
commitment and serialization path without VM execution or RocksDB
sync. The network experiments below include both.

| Probe | Median before / after | Allocation or reallocation calls before / after | Requested bytes before / after |
| --- | ---: | ---: | ---: |
| 64 payments | 177.70 / 146.22 µs | 1,164 / 397 | 103,826 / 72,078 |
| 256 payments | 605.01 / 566.68 µs | 4,622 / 1,551 | 416,018 / 288,846 |
| 1,024 payments | 3,045.95 / 2,369.56 µs | 18,448 / 6,161 | 1,664,786 / 1,155,918 |
| 1,024 entries, mixed | 4,780.84 / 3,996.79 µs | 18,718 / 6,295 | 4,103,700 / 2,927,520 |

The medians use four alternating processes per build pinned to CPU 2.
Allocation counting ran in separate binaries. The first allocation run
linked the candidate Merkle library into its baseline. The archived
initial counts measure only part of the change. The table uses the
clean builds linked from the pinned baseline and candidate source.
The archive includes failed rebuild attempts and both source and
library fingerprints.

The four-replica runs fixed a 250 ms cadence, a two-billion-cycle
block limit, two VM workers per replica and 64 persistent submitters.
Each pair used identical genesis and signed workloads. Visible TPS
counts transactions seen on all replicas during the offer window.
Every fully written payment in these runs finalized.

| Planned load and order | Build | Offered / admitted TPS | Visible / with drain TPS | Finalized / client drops | p50 / p95 / p99 | Four-replica CPU | Process writes |
| --- | --- | ---: | ---: | ---: | --- | ---: | ---: |
| 1,000 TPS, 30 s, first pair | Baseline | 1,000.00 / 1,000.00 | 980.10 / 984.30 | 30,000 / 0 | 0.639 / 0.807 / 0.861 s | 43.14 s | 500.94 MB |
| 1,000 TPS, 30 s, first pair | Candidate | 1,000.00 / 1,000.00 | 980.73 / 984.07 | 30,000 / 0 | 0.639 / 0.810 / 0.856 s | 45.08 s | 491.18 MB |
| 1,000 TPS, 30 s, reversed | Candidate | 1,000.00 / 1,000.00 | 977.03 / 985.98 | 30,000 / 0 | 0.640 / 0.801 / 0.839 s | 45.06 s | 505.78 MB |
| 1,000 TPS, 30 s, reversed | Baseline | 853.87 / 851.73 | 808.03 / 827.31 | 25,616 / 4,384 | 1.281 / 2.131 / 2.444 s | 80.99 s | 419.81 MB |
| 2,000 TPS, 20 s | Baseline | 930.00 / 926.80 | 861.60 / 882.98 | 18,600 / 21,400 | 1.734 / 2.320 / 2.613 s | 68.80 s | 332.25 MB |
| 2,000 TPS, 20 s | Candidate | 943.05 / 939.85 | 861.45 / 902.35 | 18,861 / 21,139 | 1.695 / 2.518 / 2.824 s | 61.43 s | 333.13 MB |
| 750 TPS, 20 s, 2 KiB Data | Candidate | 693.00 / 689.80 | 642.35 / 668.21 | 13,860 / 1,140 | 1.496 / 2.087 / 2.246 s | 55.15 s | 567.28 MB |
| 750 TPS, 20 s, 2 KiB Data | Baseline | 740.45 / 739.50 | 699.90 / 708.77 | 14,809 / 191 | 1.260 / 1.607 / 1.776 s | 54.98 s | 645.41 MB |

In the matched 30-second pair, peak summed replica RSS was
626.13 MiB before and 625.57 MiB after. Aggregate network bytes
sent and received were 81.74 MB and 83.28 MB, respectively. The
overload runs had similar visible rates and different p99 times.
The reverse sustained pair and the 2 KiB pair reached different
offered rates. These samples show no repeatable improvement in
replicated payment throughput or tail latency. Allocation savings
in the isolated path remain distinct from disk write savings.

A mixed-version four-replica network finalized 96 signed payments
with a replica offline, then recovered the same Header after restart.
The Lineage regression serially replayed all 2,130 finalized commands
and 4,260 Head updates. The client dropped 750 scheduled commands
before writing them. The Keeper, Intercell, xUDT, fee payout, funded
committee handoff, clustered Spore and shared Metatype quorum
regressions passed. A freshly built WASM verifier checked archived
and new CKB devnet rounds for inclusion of the signed transaction
and its execution receipt. It rejected a changed transaction and a
changed receipt in each checked proof.

The release workspace passed 494 tests, the Go proof suite passed,
and the frozen CometBFT feature built. [Raw experiments, signed workloads, executable fingerprints, and reproduction scripts](benchmarks/2026-11-26-stem-execution-merkle.json.xz).

## Shared Jolteon receive buffers

Baseline `ef12ab1` and candidate `b9edda9` used the same release
profile, CKB Lock ELF and i9-13900H host. The candidate shares the
immutable TCP receive allocation through protobuf frame decoding,
proposal decoding and validated Strand transactions. Both builds
re-encode protobuf messages to check canonical bytes and validate
the complete Strand transaction. The candidate also passes existing
transaction buffers to its proposal and client encoders.

The pinned-core probe used actual signed ordinary payments and signed
payments carrying 2 KiB of Cell Data from the preceding devnets.
It placed the transactions in proposals with fixed metadata
and measured canonical buffering, decoding and Strand parsing,
before script verification. Frames and ordered transaction IDs
were identical. Four time samples per build ran in alternating
order on CPU 2. Allocation counting ran separately and excluded
the initial receive buffer.

| Decode fixture | Encoded frame | Median before / after | Allocation calls before / after | Requested bytes before / after |
| --- | ---: | ---: | ---: | ---: |
| 64 ordinary payments | 16,202 B | 198.18 / 148.38 µs | 660 / 400 | 143,481 / 63,609 |
| 1,024 ordinary payments | 251,403 B | 3.982 / 3.016 ms | 10,268 / 6,168 | 2,261,242 / 1,006,330 |
| 64 payments with 2 KiB Data | 147,275 B | 333.46 / 202.36 µs | 660 / 400 | 1,060,986 / 325,754 |
| 1,024 payments with 2 KiB Data | 2,348,556 B | 14.583 / 9.738 ms | 10,268 / 6,168 | 16,941,307 / 5,200,635 |

The four-replica tests used two VM workers per replica, a 250 ms
proposal interval, a two-billion-cycle block limit and 64 persistent
client senders. Each pair used identical funded genesis and signed
workloads. Visible TPS measures all replicas during the offer window.
The host was shared. CPU pressure and client drops were especially
pronounced in the first runs.

| Planned load and order | Build | Offered / admitted TPS | All-replica visible / with drain TPS | Finalized / client drops | p50 / p95 / p99 | Replica CPU |
| --- | --- | ---: | ---: | ---: | --- | ---: |
| 1,000 TPS, 30 s | Baseline | 558.47 / 556.33 | 525.43 / 537.80 | 16,754 / 13,246 | 2.242 / 3.092 / 3.533 s | 75.46 s |
| 1,000 TPS, 30 s | Candidate | 575.33 / 573.20 | 529.10 / 559.15 | 17,260 / 12,740 | 2.230 / 3.141 / 3.815 s | 78.49 s |
| 750 TPS, 20 s, 2 KiB Data, first pair | Candidate | 593.35 / 590.15 | 536.60 / 559.84 | 11,867 / 3,133 | 1.999 / 2.769 / 3.692 s | 55.06 s |
| 750 TPS, 20 s, 2 KiB Data, first pair | Baseline | 631.40 / 628.20 | 560.35 / 590.65 | 12,628 / 2,372 | 1.833 / 2.532 / 2.798 s | 55.78 s |
| 750 TPS, 20 s, 2 KiB Data, reversed | Baseline | 555.40 / 552.20 | 491.35 / 530.22 | 11,108 / 3,892 | 2.111 / 2.863 / 3.343 s | 50.97 s |
| 750 TPS, 20 s, 2 KiB Data, reversed | Candidate | 630.20 / 627.00 | 562.90 / 592.68 | 12,604 / 2,396 | 1.785 / 2.422 / 2.617 s | 58.22 s |
| 2,000 TPS, 20 s | Baseline | 1,022.15 / 1,018.95 | 933.15 / 984.95 | 20,443 / 19,557 | 1.390 / 2.443 / 2.767 s | 61.36 s |
| 2,000 TPS, 20 s | Candidate | 1,438.50 / 1,436.40 | 1,328.05 / 1,391.27 | 28,770 / 11,230 | 1.237 / 1.722 / 2.007 s | 65.23 s |
| 1,000 TPS, 24 s, candidate first | Candidate | 1,000.00 / 1,000.00 | 974.58 / 981.07 | 24,000 / 0 | 0.835 / 1.072 / 1.133 s | 57.12 s |
| 1,000 TPS, 24 s, candidate first | Baseline | 1,000.00 / 1,000.00 | 973.79 / 981.09 | 24,000 / 0 | 0.681 / 0.882 / 1.005 s | 42.16 s |
| 1,000 TPS, 24 s, baseline first | Baseline | 1,000.00 / 998.75 | 969.50 / 976.03 | 24,000 / 0 | 0.618 / 0.777 / 0.813 s | 33.43 s |
| 1,000 TPS, 24 s, baseline first | Candidate | 1,000.00 / 1,000.00 | 973.17 / 983.23 | 24,000 / 0 | 0.607 / 0.767 / 0.800 s | 32.51 s |

The large-data pairs reversed direction when run order reversed.
The overload pair reached different offered rates while observed
host CPU pressure fell. Both effects prevent a causal estimate of
the throughput change from those runs. The archive includes
process writes, sampled memory, network traffic, and host CPU
pressure for interpreting the comparisons.

The two 24-second pairs finalized 24,000 transactions per run
without client drops. Visible throughput was close across the four
runs. The change in CPU and p99 reversed between pairs, so neither
metric showed a repeatable improvement. A separate probe repeatedly
read four decoded transaction fields from 1,024 transactions. It
compared a shared proposal allocation with one allocation per
transaction. Two readers pinned to separate cores took a median
791.83 versus 276.80 ms for ordinary payments, and 907.23 versus
269.28 ms for payments with 2 KiB Data. This isolates a reference
count contention cost under repeated field reads. Its contribution
to the network runs remains unmeasured.

Mixed binaries agreed after restart in four- and sixteen-replica
Jolteon networks. Each finalized all 96 signed payments while
one and two replicas, respectively, were offline. The distributed
AMM regression finalized all 2,880 offers and serially replayed
5,760 Lineage Head updates. Keeper, Intercell, xUDT, fee payout,
funded committee handoff, shared Metatype quorum, and clustered
Spore regressions passed on the same candidate.

The locked release workspace passed 495 tests. The Go proof suite
and frozen CometBFT build check passed. The archive retains the
initial Rust build failures from converting protobuf byte fields
and the passing receive-buffer and canonical decoding tests.

[Raw run records, signed workloads, build fingerprints and reproduction scripts](benchmarks/2026-11-26-stem-consensus-bytes.json.xz).

## Canonical Lineage references

The four-replica Jolteon integration test advanced a single Lineage from
version 0 to version 4. Each replica returned the same state proof for
`Latest` and `Exact(4)` at the new Head and rejected `Exact(0)` there.
The Store regression also rejected a stale write later in the same block.
Node.js replayed the two finality certificates and both state paths per
Header through the compiled WASM SDK. `Latest` occupied 33 bytes.

The archive includes the development run, a publication recheck, signed
proof bundles, public test trust anchors, workspace output and build
fingerprints. Reproduce with the four-replica integration test and the
WASM proof runner described in [Operations](OPERATIONS.md).

[Raw evidence](benchmarks/2026-11-26-stem-lineage-ref.json.xz).

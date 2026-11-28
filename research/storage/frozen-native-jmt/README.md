# Frozen Native and JMT comparison

| Arm | Frozen revision | Cell storage |
| --- | --- | --- |
| Baseline | `2b534a6` | Deployed Patricia and default Cargo features |
| Native | `7508258` | Patricia with the integration feature bundle |
| JMT | `880c9f1` | Fixed physical root, observed retirement and bounded mutation memo |

The audit binary adds the recovery ancestry correction at `4a2eb04`.
This correction is confined to the offline audit. The recovered audit
walk checks the certified durable tip and each stored ancestor through
the measured Header itself. The regression used the same stopped
baseline image with its verified Header and then a deliberately wrong ID.
The correct ID passed and the wrong ID was rejected.

The all-feature release workspace runs passed 603 Native and 604 JMT
tests. Each suite left 30 explicitly ignored tests outside the default
run. The matrix separately ran the ignored external measurement and
offline verifier fixtures. The initial Native failure was in the
forced-restart regression's ordered prefix and its expectations for
two combined scheduling features. The JMT failure also exposed
a real control-path defect. A Cell created in an uncommitted parent
could be absent from the raw journal, leaving its spent record
behind after commit. Record retirement now checks the prepared
OutPoint delta before reading disk.

## Workload and controls

Four validators shared a Ryzen 9 7950X3D. Their logical CPU masks were
`2,3,18,19`, `4,5,20,21`, `8,9,24,25` and `10,11,26,27`.
The generator used `0,1,16,17`. Each node had two VM workers, a
256 MiB RocksDB cache and a 512 MiB inline Live Cell index. The
verification cache had 131,072 entries. Three nodes kept 64 heights
and the fourth served archive history. The block limits were
2,048 transactions and four billion cycles, with a 100 ms proposal
interval and a 2.4 s view timeout. All arms used synchronous durability.
Patricia-only resident node indexes were disabled so both engines
ran with the same configured indexes and memory limits. JMT also
used its 1,024-node mutation memo within each update.

The signed payment corpus offered 270,000 payments at 3,600/s for 75 s
and drained for 30 s. The mixed corpus offered 77,000 payments at
2,200/s for 35 s and drained for 25 s. Mixed transactions exercised
zero through twelve Type Groups across three scripts. Each workload
ran baseline, Native, JMT, JMT, Native, baseline from fresh databases.
The pressure probe used the same settings at 5,200/s for 60 s with 35 s
of drain in baseline, Native and JMT order. Each workload used one
prepared corpus and topology across its arms. The exact commit IDs,
feature sets, resource limits, workload digests and fingerprints are
in `evidence.json`. JMT and Native are separate frozen source trees.
The engine comparison therefore also includes the differences in
their integration commits and cannot isolate the tree in the live
replicated results.

## Replay and interruption

After each measured interval the driver stopped the archive validator
with SIGKILL and kept the other three validators running. The archive
reopened the same data directory and reached the height already
reported by the remaining quorum. Offline replay regenerated the exact
signed admissions and verified the measured Header ancestry and
finality certificate. The stopped-store audit then authenticated the
recovered tip and its ancestry from the measured Header, independently
rebuilt the complete Live Cell commitment, and generated and verified
512 membership and 512 absence proofs. The matching content digests
cover the same Cell keys and bodies across engines. Native and JMT
commit different state roots and encode different Cell proofs.
The interruption test exercises a live process restart and local WAL
recovery. It does not model a device losing unwritten data.

## Profiling

Kernel perf sampling was unavailable with the host's current
permissions. We did not change the host settings. The sampled runs use
thread CPU timers and the interrupted userspace program counter at
approximately 43 samples per CPU second. Samples are taken from
five to thirty seconds into the offer and resolved against the
binary and process mappings. The sampler does not unwind stacks.
Kernel CPU may appear at the userspace return location, so unresolved
libc samples and isolated leaf names are not sufficient to identify a
subsystem. The stage histograms and driver CPU counters provide
separate timing evidence. Leader, Follower and writer stages can
overlap. Their elapsed times cannot be added to calculate finality.

## Results

The p99 is the observed write-to-common latency. CPU is the sum of
validator process time over the driver interval, which includes
funding and drain. All 3,018,000 offers across the fifteen runs were
acknowledged and commonly confirmed. Replay authenticated 7,188
archived blocks. The recovered Live Cell content digests matched
across all arms of each workload.

| Workload | Arm | Common TPS during offer | p99 | Validator CPU |
| --- | --- | ---: | ---: | ---: |
| 3,600/s payments | Baseline | 3,567–3,571 | 0.920–0.988 s | 333.5–333.7 s |
| 3,600/s payments | Native | 3,573–3,573 | 0.671–0.868 s | 320.6–320.9 s |
| 3,600/s payments | JMT | 3,575–3,580 | 0.671–0.706 s | 275.1–275.7 s |
| 2,200/s mixed | Baseline | 2,162–2,163 | 0.809–0.923 s | 131.3–131.7 s |
| 2,200/s mixed | Native | 2,169–2,171 | 0.685–0.790 s | 129.4–130.3 s |
| 2,200/s mixed | JMT | 2,166–2,169 | 0.669–0.676 s | 122.9–123.5 s |
| 5,200/s payments | Baseline | 5,085 | 3.287 s | 381.2 s |
| 5,200/s payments | Native | 5,138 | 0.922 s | 370.0 s |
| 5,200/s payments | JMT | 5,119 | 1.251 s | 321.2 s |

At 5,200/s the three arms had 6,874, 3,591 and 4,849 acknowledged
payments still pending at the offer cutoff. Each drained to zero.
There was one observation per pressure arm. These runs establish
comparable useful throughput at the tested rates and leave the
saturation ceiling open.

| Pressure stage mean | Baseline | Native | JMT |
| --- | ---: | ---: | ---: |
| Leader build | 39.01 ms | 28.93 ms | 33.33 ms |
| Follower prepare | 72.96 ms | 49.99 ms | 56.45 ms |
| Follower Cell tree | 22.91 ms | 19.37 ms | 22.90 ms |
| Durable writer | 14.16 ms | 11.45 ms | 9.47 ms |
| Control queue | 2.34 ms | 1.58 ms | 1.38 ms |

The Follower Cell tree remains the largest separately timed Cell
substage. Storage and VM functions both appear in the sampled hot
paths. The Native writer made more WAL syncs and wrote more WAL bytes
under pressure than baseline. JMT wrote more WAL bytes still, while
using 17.4% less mean CPU in the two 3,600/s repeats and about 35.2%
fewer proof bytes. The exported flush and compaction counters were
zero in the pressure reports, so they cannot explain the CPU
difference or establish physical write amplification.

## Decision

Expose the measured Native bundle through `native-v1-profile`.
It preserves the Patricia proof format and reduced preparation and
tail latency in both workload orders. The bundle remains opt-in.
Its extra scheduling and storage features need component ablations
before a default change, and the pressure result has one observation.
The JMT branch retains its verified process recovery and proof
improvements as research evidence. A JMT activation would require
versioned Cell commitments and a separately verified CKB and
Intercell path.

After removing pinned MultiGet, the full Native workspace passed 602
tests. The selected profile passed its 57 library regressions and
built in release mode. Four replicas built with this feature
confirmed and replayed all 14,400 offers in a short control run.
The killed archive reopened and passed exact-tip and Cell proof
verification. The corrected audit branch passed 604 all-feature
workspace tests.

The next Native experiment should isolate authenticated Cell tree
work and durable writing at the same proposal shape. Compare packed
Patricia journal and writer options one factor at a time. Measure
Cell tree CPU, logical bytes, WAL syncs, SST output, and finality on
full blocks of the same size. Mixed-contract work should separately
isolate decoding and VM trace reuse. The current flat CPU samples
do not identify the caller of each RNG or libc sample.

## Retired prefetch candidate

The pinned MultiGet path was compared directly with the existing
parallel pinned point reads on the same warmed disk-backed
65,536-Cell store. The two-workers test used the same CPU mask,
256 MiB RocksDB cache, and 2,048 deterministic windows per timed
round. Keys were drawn across the live range and an additional
one-eighth absent range. The readers returned identical sampled
canonical records. Test order was point, batch, batch, point twice.

| Keys per window | Point median | MultiGet median | MultiGet change |
| --- | ---: | ---: | ---: |
| 4 | 6.393 ms | 6.791 ms | +6.2% |
| 16 | 20.415 ms | 26.646 ms | +30.5% |
| 64 | 67.420 ms | 111.694 ms | +65.7% |

The candidate was slower at every width. Its prior one-call variant
had also lost to parallel reads. The removed code includes the
prefetch loader and separate column-family opening path. The
matrix Native bundle already used point reads. Raw timing
observations are retained in `evidence.json`.

## Reproduction

Build all three frozen revisions using separate Cargo target
directories. Use `cargo test --locked --release --workspace
--all-features --no-fail-fast` for runtime regression. Build the
comparison binaries with `cargo build --locked --release --bin stem`
and the feature sets in `evidence.json`. The equivalent Native build
at the result revision is:

```sh
cargo build --locked --release --features native-v1-profile --bin stem
```

Prepare each workload with the
[external driver](../external/README.md#prepare-the-trial) from the
JMT revision. Use `--ready-senders --reuse-submissions --complete-volume`,
`--block-transactions 2048 --block-cycles 4000000000` and the exact
values in the plans. Each node uses `--rocksdb-block-cache-mib 256
--live-cell-index-mib 512 --storage-metrics` and either
`--history-window 64` or `--archive`. Use `STEM_RESEARCH_JMT=fixed-root`
on JMT nodes and `--engine jmt` for its preflight and replay.
Run `measure` with all four replica PIDs. After reopening the killed
archive and stopping the committee, run `verify --verified-output`.
Pass that result's Header, genesis and height to
`research-cell-audit --proof-batch-width 32` using the corrected audit
binary. Keep only the compact evidence after cleaning disposable
databases, profiles and build outputs.

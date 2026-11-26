# Moving one immutable digest before serial admission

The candidate computes the BLAKE2b ID of an ordinary size-bounded SUBMIT frame on its receiving transport task. It hands the digest and the same immutable body to the replica. Duplicate handling, mempool limits, transaction decoding, store checks, fees, relay traffic and responses still use the original authoritative path. There is no wire or queue-size change. The reference is fefb8cc and the isolated candidate is that revision with prehash.patch applied.

Sixteen local validators used CPUs 4–31, and the independent client process used CPUs 0–3 on the same Ryzen 9 host. A 250 ms cadence and 32 senders offered 7,200 pre-signed commands on one hotspot AMM for 60 seconds at 120 planned TPS. Each reference/candidate run used the same genesis and signed input file. Two AB/BA pairs used the open-loop queue and drop policy. Serial archive replay validated the applied commands, fees, exact cycles, Head updates, proofs and asset conservation. Each run verified the common Header and certified outcome at its observation height.

| Run order | Common-confirmed after drain | Driver drops | Common-visible TPS during offer | Common p99 (s) | Replica CPU seconds | Client response p99 (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Reference 0 | 7,131 | 69 | 117.48 | 1.866 | 135.59 | 86.6 |
| Prehash 1 | 7,012 | 188 | 115.23 | 2.667 | 132.14 | 654.1 |
| Prehash 2 | 7,200 | 0 | 117.70 | 1.669 | 141.69 | 75.7 |
| Reference 3 | 7,200 | 0 | 118.37 | 1.781 | 136.08 | 98.3 |

The last two runs completed every signed command. Prehash used more replica CPU, had lower common p99, and had slightly lower offer-window visibility. The first comparison shows different realized admission counts and the opposite tail direction. The host was shared with ordinary background services and a short build during the second short candidate run. Its archived harness digest is taken from the actual running image because the on-disk harness was rebuilt. The path hash at archival time is separately retained.

**Decision from the minute-long runs.** Keeping the reference is appropriate given the small, inconsistent differences. A passive trace of queue dwell and candidate block verification is a better basis for deciding where to move work. No stage-specific vote-delay, wide-area throughput or failure-under-saturation claim follows from this comparison.

## Replay

Use the [paired native-load build instructions](../storage/README.md) to build the pinned reference, the candidate with prehash.patch, the native test harness and the RISC-V contracts. Select the same disjoint client/replica affinity on the host. Run against alternating binaries and separate output files.

```sh
python3 research/storage/run_load.py --harness /path/to/native_store-test --binary /path/to/reference --contracts /path/to/riscv-contracts --time /usr/bin/time --scratch /tmp/stem-load --output /tmp/short-ref.json.xz --mode hotspot_amm --replicas 16 --senders 32 --rate 120 --seconds 60 --drain 16
python3 research/storage/summarize.py research/admission/*.json.xz
```

The archives include the signed-workload digest, per-command client dispatch/write/response times, per-node admission and execution counters, common block observations, serial lineage audit, history-index proof and process samples.

## Ten-minute matched-volume follow-up

The next pair used 40 planned commands per second for 600 seconds with a 24-second drain, a 750 ms proposal cadence and the same 16 validators. The slower cadence permits the complete 10-minute horizon under the fixture's existing height-1000 signed expiry guard. The 60-second and 600-second runs therefore answer different load questions.

| Binary | Identical signed IDs confirmed | Driver drops | Common-visible TPS during offer | Common p99 (s) | Client response p99 (ms) | Replica CPU (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Prehash | 24,000 | 0 | 39.878 | 3.544 | 42.65 | 545.44 |
| Reference | 24,000 | 0 | 39.798 | 3.944 | 325.07 | 549.51 |

Every transaction's ID appeared in both common-finality histories. Each replay verified the common pool at version 24,000 and the other 64 trader Heads, with 48,000 Head updates total and 48,000,000 in fees. The two live schedules reached different final heights. The reference had one transient status-sampling error, then completed and verified common finality. A 40-TPS offered load leaves little room for interpreting the 0.080 TPS difference. The lower client response p99 warrants a repeated, host-isolated comparison. This longer pair does not overcome the opposite short-run tail movement or establish increased saturation capacity.

Replay using the command above with `--rate 40 --seconds 600 --cadence-ms 750 --drain 24`. The files long-prehash.json.xz and long-ref.json.xz contain all client rows, sampled Headers, audits and counters. The long-summary.json file includes the cross-run finalized ID set comparison.

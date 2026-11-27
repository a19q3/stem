# Proposal cadence, block capacity and payment dispatch

Four Jolteon validators processed a fixed corpus of 540,000 individually signed, independently spendable payments. The driver scheduled 3,000 per second for 180 seconds and observed every replica for another 45 seconds. Window TPS counts payments visible on all four validators by the deadline. The last certificate, the ordered Header chain and the signed payment roots are independently replayable after shutdown. [External driver and baseline](../external/README.md) · [Timing analysis](analyze.py)

```mermaid
sequenceDiagram
    participant D as Paced payment driver
    participant V as Receiving validator
    participant P as Proposer
    participant O as Four-replica observer
    D->>V: Write a signed payment
    V-->>D: Acknowledge admission
    P->>P: Timestamp the Header and build the candidate
    P->>O: Finalized block becomes visible on all four replicas
    O->>O: Match the signed payment to a block and its first common observation
    Note over D,O: Window throughput ends at 180 seconds; later confirmations count in drain
```

## Measured profiles

| Profile | Socket writes | Acknowledged and eventually confirmed | In 180 s | Window TPS | Confirmed after 180 s | Write-to-common p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [250 ms stock, original](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/external/540k.json.xz) | 361,707 | 361,707 | 309,720 | 1,720.67 | 51,987 | 187.15 s |
| [250 ms stock, repeat](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/repeat250.json.xz) | 354,848 | 354,848 | 305,684 | 1,698.24 | 49,164 | 184.70 s |
| [100 ms stock](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/c100.json.xz) | 376,158 | 376,158 | 374,391 | 2,079.95 | 1,767 | 1.32 s |
| [100 ms stock, repeat](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/repeat100.json.xz) | 379,751 | 379,751 | 377,936 | 2,099.64 | 1,815 | 1.25 s |
| [250 ms wide](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/wide250.json.xz) | 341,063 | 341,063 | 338,321 | 1,879.56 | 2,742 | 1.90 s |
| [100 ms wide](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/wide100.json.xz) | 369,182 | 369,182 | 367,659 | 2,042.55 | 1,523 | 1.39 s |
| [100 ms stock, 128 shared](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/send128.json.xz) | 424,821 | 403,724 | 400,430 | 2,224.61 | 3,294 | 3.25 s |
| [100 ms stock, 128 split equally](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/shard128.json.xz) | 402,468 | 402,468 | 392,372 | 2,179.84 | 10,096 | 77.40 s |

Each row uses a fresh database, 2,110 prefunding splits and a fixed network and committee. The workload commitment is `54db5ad9c4dd14032ffccef652b6ce7137aeecd17b0661d5c9fd7201bf8ceadd`. The 1,024-transaction / 2-billion-cycle profile and the 2,048 / 4-billion profile have different genesis IDs. A sampled ordinary payment cost 1,920,477 native cycles. Keeping the 2-billion-cycle limit would fit at most 1,041 of these payments even with a higher count limit. The wider profile therefore changes both limits together. [Exact profiles, artifact hashes, per-node RocksDB deltas and client errors](runs.json)

### Where the queue accumulated

In the earlier stock run, 302 of the 303 nonempty blocks seen in the offer window contained exactly 1,024 payments. Among the 51,987 payments first seen during drain, the median time from socket write to admission acknowledgement was 0.3 ms. Acknowledgement to the containing Header's timestamp took approximately 105.39 s, followed by about 1.37 s until the block was visible on all four replicas. The 100 ms stock trial recorded a median of 209 ms and 551 ms for these last two stages among its 1,767 drain payments. Across the entire offer window its median nonempty block held 706 transactions. [Original stock archive](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/external/540k.json.xz) · [100 ms stock archive](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/proposal-cadence/c100.json.xz)

The Header carries the proposer's millisecond timestamp before candidate construction. Some acknowledgements arrive after that timestamp. This is an approximate division of delay. It cannot identify an exact mempool residence or execution-completion time. The original run uses the mtime of its immediately preceding `before.json` write, 1795725184037938 Unix microseconds. The later runs bracket two wall-clock readings with the driver's monotonic clock and report the change in offset. Cross-host timing analysis also needs measured clock offsets and uncertainty for each proposer. Window TPS and write-to-common latency use only the observer's monotonic clock.

### Dispatch concurrency

With 128 shared senders, 424,821 payments reached a socket write. Of those, 20,907 connections reset before a successful response and 190 returned an incomplete response. None of those 21,097 payments appeared in the measured history. Each of the four receiving endpoints contributed between 5,011 and 5,510 of the missing confirmations. The four dedicated pools used 32 senders and 32 queue slots per endpoint, preserving both global totals. All 402,468 writes then received acknowledgements and finalized. Window throughput fell from 2,224.61 to 2,179.84 TPS and write-to-common p99 rose from 3.25 s to 77.40 s. Each 128-sender profile has one trial. The lower-concurrency 100 ms configuration had two runs, both with all writes confirmed and p99 below 1.33 s. [Minute counts, per-endpoint results and latency stages](timings.json)

The two stock 250 ms runs delivered 1,698–1,721 window TPS. Both stock 100 ms trials exceeded 2,079 TPS. At 100 ms, doubling the genesis limits produced 2,042.55 TPS in one run. These measurements support carrying the shorter interval into a multi-host comparison. They give no measured throughput advantage to the larger block profile at that cadence.

The same signed workload on a Btrfs volume spanning two NVMe devices produced 1,687.41 TPS at 250 ms and 1,837.26 TPS at 100 ms. Across the full 225 seconds, the shorter cadence increased scope-attributed device writes by approximately 10%. Both on-disk archives include certified histories and sampled native receipts. [Device measurements and payment evidence](nvme/README.md)

## Reproduce

The CPU was a 16-core AMD Ryzen 9 7950X3D. Four validators shared twelve physical cores (2–7, 10–15 plus their SMT siblings 18–23, 26–31). The driver used two (8–9, 24–25). The remaining cores ran auxiliary builds, proof replays and small CKB fixtures during parts of the measurements. The processes shared memory, cache and a tmpfs-backed database directory. Three validators retained 64 heights and the last kept archive history. Each had two VM workers. The host and loopback network limit conclusions about physical storage and distributed throughput. Brief archive RPC sampling in the first 100 ms run also shared this host. The public testnet performance gates remain open.

Follow the funded-driver instructions using `--rate 3000 --seconds 180 --drain 45`. Select the [100 ms topology](100ms-peers.json) or [250 ms topology](../external/540k-peers.json). For the wide genesis add `--block-transactions 2048 --block-cycles 4000000000` to `prepare`. For the 128-sender trials add `--senders 128`, adding `--shard-senders` only for per-endpoint pools. Use the preparation matching the archive with `drive.py verify`. The new archives contain proofs for the first, middle and last payment of one sampled block. The older 250 ms reference predates receipt sampling. Replay checks the witnessed payment and each receipt against the certified Header. Subsequent measurements capture these witnesses automatically after the timed run. [Replay instructions](../external/README.md#measure-and-replay)

```sh
python3 research/storage/proposal-cadence/analyze.py \
  --legacy-start-unix-us 1795725184037938 \
  research/storage/external/540k.json.xz research/storage/proposal-cadence/*.json.xz
```

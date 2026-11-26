# Reusing previously decoded candidate transactions

Sixteen real local Jolteon replicas replayed signed hotspot AMM commands. A follower hashed each complete proposal transaction, looked it up in its local mempool, compared all bytes including witnesses, and reused its decoded transaction on equality. Each validator still resolved inputs, ran the current branch, and compared the signed execution Header. The candidate retained the wire hashes until finalization. Leaders kept their existing prepared proposals.

| Offered TPS for 60 s | Matched finalized commands per run | Candidate verification mean, reference → reuse | Common-finality p99, reference → reuse | Common-visible TPS during offer, reference → reuse |
| --- | ---: | ---: | ---: | ---: |
| 45 | 2,700 | 7.539 → 7.437 ms | 653.8 → 655.4 ms | 44.70 → 44.63 |
| 100 | 6,000 | 16.377 → 16.325 ms | 738.8 → 736.6 ms | 99.07 → 98.88 |

Both pairs confirmed identical signed ID sets, charged identical cycles and produced two Head updates per command. The standalone serial replay audited all four runs. The 12-second counter run observed 1,012 reuse hits and 7,088 misses. The main trials used the original metrics reader, which kept latency histograms and omitted the two new counters. The counter run used a feature-aware reader and is used only for the hit rate. All loads used same-host tmpfs databases. There was one paired observation per rate. Keep the reference path as the default.

Start at commit `5f13b05`. Use a stock release node and apply `reuse.patch` to a second checkout. Build that node with `cargo build --locked --release --features jolteon-decoded-candidate-experiment --bin stem`. Use the [replicated-load runner](../storage/run_load.py) with `--mode hotspot_amm --replicas 16 --seconds 60 --senders 32 --drain 36` and the corresponding offered rate. Use `--complete-volume` at 45 TPS. The original binaries and fixture hashes are in the compressed reports. The [summary](summary.json) contains common-finality, proposal, verification, commit and process counters.

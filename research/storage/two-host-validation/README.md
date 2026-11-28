# Two-host validation on a workstation and laptop

This experiment runs two validators on a Ryzen 9 7950X3D workstation
and two on a Core i9-13900H laptop. All four use the same release binary
from the bounded-residency change at `9b3fbf4`. The workstation and
laptop communicate over direct IPv4 on the same Wi-Fi LAN. SSH launches
the laptop processes and retrieves diagnostics. Validator traffic uses
the native TCP sockets.

The workstation assigns six physical cores and their SMT siblings to
each of its two validators, on separate CCDs. Its load driver uses two
other physical cores. The laptop assigns three P-cores and their SMT
siblings to each local validator, with diagnostics on E-cores. Both
machines run x86-64 and glibc 2.44. Eight preflight ICMP replies ranged
from 3.5 to 122.5 ms, averaging 42.9 ms.

## Reproduction

Prepare the same signed independent-payment corpus and clone the same
authenticated funded state used for the 120-second single-host residency
experiment. Transfer each laptop replica's own stopped store, genesis
and fixture key over SSH. Verify the transfer, reopen the stores with
16 MiB mempools and the same two index and RocksDB cache budgets, and
check common certified finality before the offer. On the laptop use P-core
masks `0–5` and `6–11`. On the workstation use
`2–7,18–23` and `10–15,26–31` for the validators,
and `0–1,16–17` for the load driver.

Use 8,500 scheduled payments/s for 120 seconds and a 35-second drain.
Score seconds 20–60 and 80–120. Capture per-host process CPU and
per-replica status, then verify the signed archive in an independent
process. The test measures heterogeneous 2+2 deployment. The machine
placement, physical core budget and network path all differ from the
single-host reference. [Single-host method and comparison](../index-residency/README.md)

## Results

| Wall-clock window | Actual writes/s | Acknowledgements/s | Common TPS | Full nonempty blocks | Conditional p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 20–60 s | 8,508 | 2,883 | 2,560 | 50 / 50 | 123.29 s |
| 80–120 s | 8,501 | 2,423 | 2,355 | 46 / 46 | 62.88 s |

The driver wrote all 1,020,000 requests. During the 120-second offer
289,827 transactions reached common finality, or 2,415 TPS. By 155
seconds all replicas had observed 373,795 confirmations. Acknowledged
work still pending measured 113,530 at the end of the offer and 29,657 at
the drain cutoff. Capacity refusals dominated the submission errors.
Conditional p99 includes only acknowledged offers confirmed by the
cutoff. All four replicas recorded zero local view timeouts and
kept both indexes active throughout the trial.

| Host | Mean Leader prepare, late | Mean Follower prepare, late | Mean control queue, late | Two-replica CPU, late |
| --- | ---: | ---: | ---: | ---: |
| Workstation | 223.5 ms | 215.4 ms | 4.6 ms | 61.1 s |
| Laptop | 161.6 ms | 227.2 ms | 1.9 ms | 55.9 s |

The same binary and signed workload on one host gave 3,994–4,915
late-window TPS with the resident index. The two-host run was slower
despite its additional validator cores and an active index. Late
Follower preparation on the laptop was close to the workstation and
single-host candidate times. The 46 full blocks in the late 40 seconds
give a mean rate of one block per 870 ms. Ingress and control queue
timers start after the frame has reached the receiver. The result
documents performance on this Wi-Fi path. Separating transport time
from consensus and hardware effects requires proposal and vote timings
or a wired repeat at the same placement.

Independent replay authenticated the signed workload, Header chain,
sampled execution proofs and terminal finality at height 796. After
shutting down the cluster, the laptop archive reopened the durable
store, rebuilt both indexes and returned byte-identical Header and
finality proof at height 796. [Measured data and digests](results.json)

# Proposal order under sustained payment pressure

The stock Jolteon proposer iterates its local mempool in ascending BLAKE2b digest order over the **complete signed wire transaction**. The network-scoped transaction ID shown in a receipt uses a different hash input. We verified the signed workload digest and joined each common-finality observation to the correct proposal key. In the three-minute stock trial, the p99 of keys starting with hexadecimal 0 was 2.55 seconds. For keys starting with f it was 196.76 seconds. Over the full trial all 327,306 accepted payments finalized.

Three candidates changed proposal selection only. Arrival order uses a linked hash map with iteration in local admission order. Rotation uses the stock tree and two disjoint ranges, starting at the bit-reversed low byte of the parent height. A third candidate applies that rotation to one in four proposals and retains the original start for the other three. The sender configuration, resource rules, block validation, certificates and durability settings were identical. Local arrival order may vary by proposer.

Four replicas used the same binaries, contracts and CPU partitions as the [independent-input storage runs](../../storage/sustained/README.md), with the selected scheduling patch applied to `fefb8cc`. Each 45-second fixed-volume trial offered 2,800 payments/s. The bounded client could continue writing during the 45-second drain. The fixed-volume trials ran in the order stock, arrival, arrival, stock, mixed, rotation, stock, rotation and two one-in-four runs. Each trial independently confirmed exactly the same 126,000 transaction IDs, same genesis and same signed workload. The three-minute pressure runs used the same 540,000 independently funded signed payments and the ordinary bounded queue. Their actual send and confirmation counts varied. Some other research processes ran on the same workstation. The reported p99 starts when the socket write completes and ends when the payment is visible to every replica. The accompanying prefix results also report delay from the planned dispatch time.

## Equal confirmed volume

Values are medians across the listed runs. CPU sums the four node processes from the pre-offer snapshot through drain. The window counts only payments visible to all four replicas in the first 45 seconds.

| Order | Runs | Window TPS | Write-to-common p50 | Write-to-common p99 | CPU seconds |
| --- | --- | ---: | ---: | ---: | ---: |
| Stock | [1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-stock1.json.xz), [2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-stock2.json.xz), [3](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-stock3.json.xz) | 1,884.71 | 1.419 s | 63.624 s | 179.57 |
| Arrival | [1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-fifo1.json.xz), [2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-fifo2.json.xz) | 1,466.11 | 18.291 s | 32.899 s | 223.45 |
| Rotate every block | [1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-rotate1.json.xz), [2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-rotate2.json.xz) | 1,486.21 | 10.821 s | 48.877 s | 210.67 |
| Rotate one in four | [1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-quarter1.json.xz), [2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-quarter2.json.xz) | 1,851.20 | 1.700 s | 60.811 s | 184.78 |
| Mixed stock and arrival | [1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-mixed.json.xz) | 1,503.16 | 11.898 s | 35.190 s | 202.18 |

## Three-minute pressure

Each row used a 3,000/s target and one run. Completed socket writes, successful acknowledgements and transactions found in the common chain are counted independently. All acknowledged payments reached common visibility. Confirmations during drain count towards the total. They are excluded from window TPS.

| Order | Written | Acknowledged | Confirmed | In 180 s | Window TPS | Write-to-common p50 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [Stock](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/sustained/prefund-3m.json.xz) | 327,306 | 327,306 | 327,306 | 293,336 | 1,629.64 | 1.316 / 173.667 s |
| [Arrival](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-fifo-3m.json.xz) | 237,965 | 237,597 | 237,620 | 223,791 | 1,243.28 | 6.548 / 11.598 s |
| [Rotate every block](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-rotate-3m.json.xz) | 260,815 | 260,573 | 260,596 | 247,347 | 1,374.15 | 4.638 / 22.823 s |
| [Rotate one in four](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/admission/order/order-quarter-3m.json.xz) | 313,739 | 313,739 | 313,739 | 288,348 | 1,601.93 | 1.591 / 139.739 s |

## Decision

Retain the stock proposal order. Arrival order reduced the matched-volume p99 by about 31 seconds, but median transaction delay rose from 1.42 to 18.29 seconds and validator CPU rose by 24%. The every-block variant also increased median delay and CPU. One-in-four rotation was closest to stock. In its matched runs p99 fell by 2.81 seconds, window throughput fell by 1.8% and CPU rose by 2.9%. In its single pressure trial p99 improved by 33.93 seconds while the client wrote 13,567 fewer transactions. The evidence establishes a real hash-prefix bias and a configurable latency tradeoff. Adoption would require a target for that tradeoff and measurements on independent hosts.

The mixed trial used arrival order on replicas 1 and 3 and stock order on 0 and 2. The four validators agreed on their final Header and the archived history proof. No benchmark changes the order accepted by a validating node. These results measure independent native payments with the documented block limits. Global FIFO across separate proposers and a multi-host performance envelope would require separate evidence.

Use the [load runner](../../storage/run_load.py) with the documented CPU sets, `--prefund-payments --rate 2800 --seconds 45 --drain 45 --complete-volume --senders 64 --device-accounting` for matched volume. For the pressure profile use `--rate 3000 --seconds 180` and omit `--complete-volume`. Build each candidate by applying its patch to `fefb8cc` and rebuilding the release node. Save one signed stream per profile with `--workload-output FILE`. Run `python3 compare.py ARCHIVES...` to check finality and matched inputs, then `python3 prefixes.py --workload FILE ARCHIVES...` on one profile at a time to reproduce the prefix distributions. [Arrival patch](arrival.patch) · [Rotation patch](rotate.patch) · [One-in-four patch](quarter.patch) · [Run summary](summary.json) · [Per-prefix latency](prefixes.json)

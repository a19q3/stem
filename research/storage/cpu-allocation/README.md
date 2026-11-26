# Validator CPU allocation under funded payment pressure

Four stock Jolteon validators shared either six physical cores (10–15 and their SMT siblings 26–31) or twelve (2–7, 10–15 and siblings 18–23, 26–31). Each validator retained two VM workers. The load generator kept the same two physical cores (8–9,24–25), 64 senders and connection policy. The workstation had 16 physical cores total. Separate wallet builds and fixture tests used the remaining two cores during parts of the study. All processes shared memory and storage. Each fixture started with independent spendable outputs confirmed on all replicas before the offer.

Four equal-volume runs used the order six, twelve, twelve, six. Every run confirmed the same 126,000 signed transactions and checked common finality and archive history. The 45-second offer used a 2,800/s target with backpressure and up to 45 seconds to drain. Window TPS counts transactions visible to all four validators during the offer. CPU sums all four validator processes over offer and drain.

| Physical cores | Raw run | Window TPS | p50 / p99 seconds | Validator CPU seconds |
| ---: | --- | ---: | ---: | ---: |
| 6 | [six1.json.xz](six1.json.xz) | 1,898.96 | 1.343 / 66.056 | 182.75 |
| 12 | [twelve1.json.xz](twelve1.json.xz) | 1,989.93 | 1.169 / 62.022 | 174.46 |
| 12 | [twelve2.json.xz](twelve2.json.xz) | 1,943.89 | 1.269 / 63.156 | 174.93 |
| 6 | [six2.json.xz](six2.json.xz) | 1,884.31 | 1.293 / 66.171 | 179.39 |

The six-core medians were 1,891.63 window TPS, 66.11 s p99 and 181.07 CPU seconds. The twelve-core medians were 1,966.91 window TPS, 62.59 s p99 and 174.70 CPU seconds.

The two 180-second trials then offered 3,000/s with the ordinary bounded client queue, one run per allocation. Their signed corpus and genesis matched, but the number of actual socket writes varied. The p99 starts at socket write and ends at all-replica visibility.

| Physical cores | Written | Acknowledged | Confirmed | In 180 s | Window TPS | p99 seconds | Peak acknowledged backlog | Device write GB |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [6](six-pressure.json.xz) | 327,592 | 327,592 | 327,592 | 298,186 | 1,656.59 | 168.019 | 29,702 | 11.177 |
| [12](twelve-pressure.json.xz) | 319,903 | 319,903 | 319,903 | 290,961 | 1,616.45 | 168.854 | 28,942 | 11.079 |

Widening the validator CPU set improved the matched-volume results. Under sustained pressure, the twelve-core run dispatched 7,689 fewer payments and its common-visible window throughput was 40.14 TPS lower. A single run per configuration leaves the cause of that difference unresolved. The experiment provides no sustained gain from allocating more node cores alone. The next capacity measurement needs an independent load generator and validator hosts. [Exact matched IDs and workload hashes](comparison.json) · [I/O, proposal and commit measurements](storage.json) · [Time-resolved backlog](backlog.json)

To reproduce, use the [funded-input runner](../run_load.py) at `fefb8cc` for the stock node with the harness from the research branch. Use `--prefund-payments --device-accounting --senders 64 --drain 45 --client-cpus 8-9,24-25` and the respective node CPU sets above. The fixed-volume profile adds `--complete-volume --rate 2800 --seconds 45`. The pressure profile uses `--rate 3000 --seconds 180`. The three tables of JSON come from [the equality checker](../../admission/order/compare.py), [storage accounting](../summarize.py) and [backlog analysis](../device/backlog.py).

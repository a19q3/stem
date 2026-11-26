# Reusing immutable Lineage transaction hashes

Store admission, materialization and candidate verification can request the same original transaction ID and signed commitment repeatedly. The decoded Lineage command now owns one lazy slot for each value for the first network used. Clones share that immutable command. Looking up another network computes the appropriate domain hash directly. Rebuilding a transaction after changing its witnesses creates a fresh decoded command. The regression checks both network domains, concurrent clones and regenerated witnesses.

| 16 local replicas, one hotspot, 200 commands/s for 40 s | Stock | Shared hashes |
| --- | ---: | ---: |
| Confirmed per run, three runs per build | 8,000 | 8,000 |
| Replica CPU seconds, median | 136.06 | 134.60 |
| Mean candidate verification, median of runs | 26.12 ms | 25.90 ms |
| Common-finality p99, median | 913 ms | 904 ms |
| Within-window common TPS, median | 196.125 | 196.125 |

The stock/candidate, candidate/stock, stock/candidate pairs used 1.77%, 0.94% and 1.07% less aggregate replica CPU with reuse. The confirmed ID sets, genesis, signed workload, charged cycles and fees matched in every run. Serial replay checked 16,000 Head updates per run. Peak sampled single-node RSS ranged from 101,072 to 101,828 KiB on stock and from 101,112 to 102,072 KiB with reuse. These six runs measure CPU for equal completed work below saturation. Throughput capacity and cross-host latency were outside this comparison. [Measurements and workload digests](summary.json)

For a 505-byte signed command on one CPU, cloning and fetching both hashes took 1,526 ns on stock and 34 ns with reuse in both interleaved pairs. Decoding fresh bytes and fetching both hashes took 5,971–5,987 ns on stock and 6,037–6,053 ns with reuse. The 31-block fixed Store benchmark produced 5.36–5.46 ms stock and 5.37–5.45 ms candidate median serial prepare. The implementation keeps both hashes scoped to one immutable command to capture the repeated-lookup benefit with bounded state.

## Reproduce

Use the parent and candidate ledger implementations with identical contracts. Build the release replica and native_store harness for each. The ignored `lineage::measure_lineage_command_hashes` test measures first-use and repeated hashes, and `lineage::measure_lineage_hotspot_amm` measures eight swaps per block. Apply the candidate's test-only changes to the stock checkout for a common harness. Set `STEM_TEST_CONTRACT_DIR` to the compiled contract directory. Pin the hash test to CPU 2 and the Store test to CPUs 4–7. Run the [load harness](../storage/run_load.py) with `--mode hotspot_amm --replicas 16 --rate 200 --seconds 40 --drain 36`. Run `python3 ../lineage-engine/compare.py *.json.xz` to verify cohorts from this directory.

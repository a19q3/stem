# Native Lineage CLOB benchmark

This fixture defines a closed test market. One existing Lineage Head holds each book. Four separately signed fee Locks identify the traders. The balances represent two genesis-funded benchmark assets held inside that Head. The fixture exposes placement and cancellation. Its ledger has no external deposits or withdrawals.

- A placement signs a side, integer limit price in quote units, and integer size in base units. The book allocates a strictly increasing order ID in canonical execution order. Buy orders reserve `price × size` quote units and sell orders reserve `size` base units.
- A crossing order matches the best available opposing price, then the lowest maker order ID at that price. It trades at the maker price, credits acquired assets, refunds the difference between reserved and actual quote for buys, and retains any unmatched remainder at the signed limit. A maker can fill across multiple transactions.
- A cancellation signs an order ID and requires the fee signer to own that resting order. It releases the entire unused reserve. A cancellation creates zero fills. An invalid operation or an unfillable remainder at capacity takes the native fee-only rejection path, with every market effect rolled back.
- The book limits resting orders to 16 per market. Price, quantity, IDs, counters, and both balances use bounded integer arithmetic. A placement crossing an order from the same owner rejects atomically. The fixture uses a fixed initial signer roster and has no order expiry rule.

Report committed order operations, applied placements, applied cancellations, fee-only rejections, match fills, and filled base quantity separately. A fill transfers both assets within the committed market Head. Compare complete Headers and receipts by native serial replay, and use a separate matching and accounting model to verify booked order IDs, priority, free balances, and conserved inventory.

## Workload and replay

Each market starts with four independently signed trader fee Locks and 10,000,000 units of each asset per trader. Book IDs 1 and 2 are a seeded ask at 101 and bid at 99, each of size two, with their collateral held. The first two scheduled operations per market cancel those seeded orders. Later placements alternate bid and ask at 100, with sizes from one to three. The fixture assigns one fresh fee Cell per operation. At four replicas, use 320 offered operations/s for 16 seconds across one or 32 markets. At sixteen replicas, use 160 offered operations/s for 16 seconds across 32 markets. Both use a 22-second maximum drain on one host. Hold the node, cadence, block budgets and fee schedule fixed within each comparison. The 32-market four-replica runs use the order default, independent prefix, independent prefix, default with byte-identical signed input.

The harness verifies the common Jolteon certificate and reopens the archive. A fresh serial Store reproduces the full Header and receipt list at each height. The independent oracle keeps orders in a map and accounts for each trader's total ownership. It derives available balances from outstanding collateral and uses sorted maker priority to compute fills. It checks every order and available balance in each touched market at block boundaries, and verifies both authenticated proof paths for every final market Head. The summarizer derives fill throughput from the common observation time of the transaction that executes each fill. Maker offer-to-fill latency uses maker offer time through common observation of the matching transaction, with seeded orders excluded from its population.

```sh
CXXFLAGS='-include cstdint' cargo build --release --locked --bin stem
STEM_TEST_EVIDENCE_ROOT=/tmp/stem-clob \
STEM_LINEAGE_WORKLOAD=clob STEM_LINEAGE_HEADS=32 \
STEM_STORAGE_REPLICAS=4 STEM_STORAGE_VM_WORKERS=4 \
STEM_STORAGE_RATE=320 STEM_STORAGE_SECONDS=16 STEM_STORAGE_DRAIN=22 \
STEM_STORAGE_CADENCE=100 STEM_STORAGE_SENDERS=64 \
STEM_STORAGE_BLOCK_CYCLES=4000000000 STEM_STORAGE_BLOCK_TRANSACTIONS=2048 \
STEM_STORAGE_COMPLETE_VOLUME=1 STEM_STORAGE_READY_SENDERS=1 STEM_STORAGE_REUSE_SUBMISSIONS=1 \
CXXFLAGS='-include cstdint' cargo test --release --locked --test native_store \
  storage_load::sustained_lineage_load -- --exact --ignored --nocapture
python3 research/clob/summarize.py /tmp/stem-clob/stem-storage-*
```

Select the optional build with `STEM_TEST_REPLICA_BIN`. For its node, add `--features native-lineage-ready-prefix` to the build command and copy the executable to a distinct path. The measuring harness uses four VM workers per replica at size four and two at size sixteen. In the paired measurements, the driver was pinned to `0-1,16-17` and nodes to `2-7,10-15,18-23,26-31`. The executable runs through the same native application dispatch and Jolteon consensus used by the AMM fixture.

## Results

| Profile | Common operations/s | Common fills/s | Confirmation p99 | Maker offer-to-fill p99 | Confirmed by drain | Verified fills |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 replicas, 1 market | 313.63 | 235.00 | 520 ms | 617 ms | 5,120 | 3,835 |
| 4 replicas, 32 markets, default A1 | 312.69 | 256.00 | 506 ms | 792 ms | 5,120 | 4,192 |
| 4 replicas, 32 markets, prefix B1 | 311.13 | 254.25 | 502 ms | 780 ms | 5,120 | 4,192 |
| 4 replicas, 32 markets, prefix B2 | 312.50 | 256.00 | 504 ms | 800 ms | 5,120 | 4,192 |
| 4 replicas, 32 markets, default A2 | 312.00 | 256.00 | 497 ms | 785 ms | 5,120 | 4,192 |
| 16 replicas, 32 markets | 156.44 | 126.00 | 478 ms | 1,072 ms | 2,560 | 2,080 |
| 4 replicas, 32 markets, 900/s pressure | 797.88 | 594.38 | 3,851 ms | 3,431 ms | 14,400 | 10,759 |

The seven trials reconciled **42,560 order operations**, **33,442 fills**, and **1,837 replayed blocks**, excluding the 128-operation smoke run. Every offered operation confirmed by drain. There were no fee-only rejections in these replicated runs. The targeted RISC-V regression separately exercised unauthorized cancellation, same-owner crossings, insufficient collateral, the 16-order depth bound, full and partial fills, and database reopen. That test also checks fee-only accounting and proofs after every operation. The 900/s trial retained the 320/s test's block settings and had 1,571 offered operations still pending at the offer cutoff. It establishes a pressure data point over a 16-second window. The paired comparison favored neither scheduler consistently, so independent prefixes remain opt-in.

The shared signed corpus digest for 32 markets at 320/s was `3dfdccf892c7742e5c269e0d3e84af0c5150c599619869c6fe726ca2bfbcb219`. SHA-256 fingerprints were `d09f92203af1b5c6c494120ac2989d1da7a362868cac0c120ff312794f9518ef` for the `4ffbce4` default node, `49dae1b8edb52a488d93ca6ec3652ed15966261bcd8d2f58592958adb24dd1fe` for its prefix build, `9b99581379f7f534174c95d2b6a06ce8bb37d88b8d1f81db340a2704dec5bfff` for the shared harness and `f48f912183dc35644e5fad3812add7230c6dbf795651001169ecb3793ed2854f` for the guest.

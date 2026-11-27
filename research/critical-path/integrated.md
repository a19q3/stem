# Trace reuse and RocksDB cache under payment pressure

The branch with [assembly trace reuse](decoder-cache.md#round-3--share-whole-verified-decode-traces) and `stem run --rocksdb-block-cache-mib 256` was compared against exact `main@eeefe97` with the stock local cache. The comparison ran A–B–B–A from fresh directories under the same four-replica placement. The branch build includes no timing probes. Each arm offered the same 270,000 independent signed payments over 75 seconds, with 256 persistent ready senders and a 30-second drain. The proposal interval was 100 ms, the timeout 2,400 ms, and the block limits 2,048 transactions and four billion cycles. The separate cache and trace studies isolate the effects held together here.

The signed workload digest was `9159a67883cda4027f53ca61cdbd184226a4f277d90da9fc630c4b002db43608`. The node SHA-256 values were `3932e122d470201ff64f4a2b07a58ca895aefefc977426b9d722a9b49df16afc` for exact `main` and `1c5236a447ff24f64378685a73ba8bd519c70a86db981c2d0ec0f414dcd11540` for the branch. Use the [same pinned topology](README.md#measurement-and-reproduction) and prepare the common fixture with the external driver.

```sh
python3 research/storage/external/drive.py --harness "$harness" --contracts "$contracts" \
  prepare --topology "$topology" --trial "$trial" --rate 3600 --seconds 75 --drain 30 \
  --senders 256 --block-transactions 2048 --block-cycles 4000000000 \
  --ready-senders --reuse-submissions --complete-volume
```

Use `verification_cache_entries=131072` in the launch directory for both builds and add `--rocksdb-block-cache-mib 256` only for the branch arm. Independently replay each report after stopping the four nodes. The table uses common confirmations during the 75-second offer, p99 over all written payments including the drain, and `schedstat` CPU in the offer. WAL figures cover offer and drain and include all four replicas.

| Run order | Actual offers/s | Common TPS | Write-to-common p99 | Validator CPU | Leader build mean | Follower prepare mean | WAL syncs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `main` A1 | 3,596.61 | 3,485.00 | 4.011 s | 351.21 s | 104.61 ms | 223.06 ms | 4,766 |
| Branch B1 | 3,600.00 | 3,535.64 | 2.624 s | 312.62 s | 66.59 ms | 152.18 ms | 5,458 |
| Branch B2 | 3,600.00 | 3,534.29 | 2.470 s | 310.57 s | 64.61 ms | 148.15 ms | 5,504 |
| `main` A2 | 3,598.63 | 3,520.95 | 3.769 s | 351.29 s | 96.55 ms | 208.78 ms | 5,012 |

Against the paired controls, common TPS increased 1.45% and 0.38%. Validator CPU fell 11.0% and 11.6%, and p99 fell 34.6% and 34.5%. Mean control queue wait fell from 31.89 to 13.83 ms in the forward pair and from 27.49 to 12.89 ms in the reverse pair. Both branch repeats ended within two TPS of each other. There were no local view timeouts. All four runs acknowledged and confirmed the entire signed corpus after drain. Independent replay verified 1,748 blocks and 1,080,000 transaction instances across the four runs.

The branch used 12.1% more WAL syncs and about 3.7% more aggregate WAL bytes. It formed more and smaller nonempty blocks, with a median 1,203–1,233 payments versus 1,500–1,585 on `main`. Only 1–3 offer-window blocks per branch run reached the transaction cap versus 26–35 in the controls. The configured proposal cadence was identical throughout. The measured throughput rises modestly at this offered rate, while execution time and latency gains are substantial in both directions. The combination is qualified here on fresh single-host databases for a 75-second pressure test. Its measurements do not establish the saturation ceiling or cross-host qualification. Neither the controlled test nor the individual optimisations change the block encoding, resource charges, vote rule or synchronous durability.

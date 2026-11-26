# Reusing RISC-V ELF loading metadata

The candidate caches at most eight successful canonical ELF64 loading plans per thread, keyed by machine version, file length and every byte of the ELF header and program header table. The machine continues loading each segment from the current program. A real guest with identical headers and different code returned 0, 7, 2, 8, 1 and 0 in sequence. The reference and candidate produced the same digest across 32- and 64-bit register types, VM versions 0–2, individual header changes, invalid lengths and buffers with a nonzero slice offset.

| Hotspot AMM, 16 replicas, 60 s | Stock → reuse, 100 TPS | Stock → reuse, 200 TPS |
| --- | ---: | ---: |
| Confirmed signed commands per run | 6,000 → 6,000 | 12,000 → 12,000 |
| Candidate verification mean | 15.95 → 16.03 ms | 32.37 → 34.47 ms |
| All-replica visible TPS in the offer window | 99.22 → 98.82 | 198.13 → 198.00 |
| All-replica p99 | 735 → 734 ms | 919 → 1,058 ms |
| Replica CPU seconds | 116.84 → 117.27 | 206.03 → 214.13 |

The parser-only experiment alternated stock/candidate/candidate/stock binaries on CPU 2 with four genuine contract images. Median nanoseconds per call over 17 samples per binary were 106.05, 22.57, 22.36 and 105.04. The end-to-end runs used nominally identical local settings and the same signed IDs in both pairs. Both nodes charged identical cycles, and the serial audit verified every pair of Lineage Head updates. Short development builds and local integration tests also used this workstation. There was one pair at each target rate. Keep the stock loader as the default.

To reproduce, start from `b97cdd7`, copy `elf_loading.rs` into `vm/tests/`, apply `reuse.patch`, and build stock and feature-enabled release binaries in separate directories. Select `--features stem-vm/elf-plan-reuse` for the candidate. Run `cargo test -p stem-vm --release --test elf_loading` for the differential test. With the four contract executables in `STEM_TEST_CONTRACT_DIR`, run each test executable on CPU 2 with `--ignored --nocapture`. Use the [native load runner](../storage/run_load.py) with `--replicas 16 --mode hotspot_amm --seconds 60 --drain 36` and the rate in the table. The [measurements and full test log](summary.json) and the four compressed network reports carry the binary hashes and finality evidence.

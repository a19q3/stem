# Translation validation for B/J immediate extraction

The benchmark extracts the pinned branch and JAL immediate helpers directly from the vendored CKB-VM source. It compares a portable mask-and-shift candidate that folds sign propagation into a single shifted mask. Z3 returned unsatisfiable for a disagreement on either 32-bit formula. A mutant that clears branch immediate bit 11 was rejected on input word 128. This proof covers the extraction expression on all 32-bit inputs. Opcode admission, decode order and complete VM execution are outside the pure-function proof.

On the built stem-lock ELF, linear parsing of .text found 263 standard branch words, one JAL, 2,836 other 32-bit instructions and 3,568 compressed instructions. The benchmark extracted the standard branch/JAL fields, kept their order and checked identical values against the direct Rust reference. With 1,024 passes per sample, alternating order and one discarded warmup pair, the reference took 0.884 ns/field and the masked candidate 1.012 ns/field on one Ryzen 9 7950X3D core. The instruction mix is static code composition. It does not report dynamic execution frequency. **Decision:** keep the reference decoder. No guest program or VM fast path was changed.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' cargo build --locked --release --manifest-path contracts/Cargo.toml --target-dir target/stem-contracts --target riscv64imac-unknown-none-elf --bin stem-lock
# Requires Python, pyelftools and z3-solver. Select an allowed CPU locally.
taskset -c 2 python3 research/immediates/compare.py target/stem-contracts/riscv64imac-unknown-none-elf/release/stem-lock
```

The archive includes the ELF digest, Z3 version, all checksum/timing pairs and CPU affinity. An ARM64 host and a dynamic runtime profile remain outside this run.

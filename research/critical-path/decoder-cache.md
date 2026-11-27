# Instruction-cache experiment

## Round 1 · Per-thread hashing of raw instruction bits

Discarded. The candidate cached successful standard RV64 factory results on a 4,096-slot thread-local table keyed by exact raw bits, ISA and VM version. Each fetch still checked executable memory. The existing PC cache, macro-op fusion, custom factory ordering and cycle accounting were kept. Only assembly code used the new cache.

Four uninstrumented binaries ran the [mixed fixture](README.md#measurement-and-reproduction) in A–B–B–A order from fresh stores. Every run acknowledged and confirmed the same 55,000 signed transactions. Actual in-window offers were 2,192.44–2,200.00/s. Independent offline replay rebuilt all four certified histories, covering 1,041 blocks and 220,000 transactions.

| Order | Common TPS | Write-to-common p99 | Validator CPU | Mean admission check | Mean Follower prepare |
| --- | ---: | ---: | ---: | ---: | ---: |
| Exact `main` A1 | 2,125.08 | 1.438 s | 95.49 s | 307.78 μs | 92.91 ms |
| Cache B1 | 2,023.52 | 2.235 s | 100.37 s | 327.37 μs | 106.99 ms |
| Cache B2 | 2,098.32 | 1.521 s | 97.24 s | 316.15 μs | 93.12 ms |
| Exact `main` A2 | 2,092.40 | 1.437 s | 95.80 s | 308.34 μs | 88.77 ms |

The paired control order means 3.3% higher aggregate CPU with the cache and roughly 4.4% slower admission checks. Follower means increased in both pairings. Throughput fell 4.8% in the forward pair and rose 0.3% in the reverse pair. p99 degraded in both. The candidate's lookup, comparison and table footprint are plausible costs during an existing PC-cache miss. The run did not separately count cache hits and misses in the added table, so it cannot identify which cost dominated. A long payment run would not justify shipping a default regression on the profiled mixed workload.

The vendored assembly suite passed 168 tests, and the locked release workspace passed 529 tests with 29 ignored. The vendor suite included a differential check of every 16-bit pattern and 16,384 deterministic 32-bit words on three VM versions and four ISA extension combinations. The matched network results verify transition and proof continuity, while the tests check decoded values and the covered reset and guest behaviors. Build from the [profiled revision](https://github.com/a19q3/stem/commit/eeefe975aede7c7aef433a2ab4639c0dfaed5a3f), apply the patch below with `git apply --unidiff-zero`, and use the default locked release build for both binaries. The candidate SHA-256 was `08504cd1068dbab1c37b7283e24e873cd10fecf685ffdf958a52344b1bb40090`. Use the same prepared trial and `drive.py verify` for all arms. The cache is omitted from the branch runtime.

<details>
<summary>Rejected candidate patch</summary>

```diff
diff --git a/vendor/ckb-vm/src/decoder.rs b/vendor/ckb-vm/src/decoder.rs
index 4b0a49b..7bae53b 100644
--- a/vendor/ckb-vm/src/decoder.rs
+++ b/vendor/ckb-vm/src/decoder.rs
@@ -2,0 +3,2 @@ use ckb_vm_definitions::registers::{RA, ZERO};
+#[cfg(has_asm)]
+use std::cell::Cell;
@@ -13,0 +16,35 @@ const INSTRUCTION_CACHE_SIZE: usize = 2048;
+#[cfg(has_asm)]
+const FACTORY_CACHE_SIZE: usize = 4096;
+
+#[cfg(has_asm)]
+thread_local! {
+    // These values depend only on the instruction bits and ISA. Executable
+    // memory checks and address-dependent fusion still happen in each VM.
+    static ASM_INSTRUCTIONS: Box<[Cell<(u64, Instruction)>]> = (0..FACTORY_CACHE_SIZE)
+        .map(|_| Cell::new((u64::MAX, 0)))
+        .collect();
+}
+
+#[cfg(has_asm)]
+fn asm_instruction(
+    bits: u32,
+    isa: u8,
+    version: u32,
+    factory: InstructionFactory,
+) -> Option<Instruction> {
+    let key = u64::from(bits) | (u64::from(isa) << 32) | (u64::from(version) << 40);
+    let slot =
+        (bits ^ bits.rotate_right(12) ^ bits.rotate_right(23)) as usize & (FACTORY_CACHE_SIZE - 1);
+    ASM_INSTRUCTIONS
+        .try_with(|entries| {
+            let entry = &entries[slot];
+            let (cached, instruction) = entry.get();
+            if cached == key {
+                return Some(instruction);
+            }
+            let instruction = factory(bits, version)?;
+            entry.set((key, instruction));
+            Some(instruction)
+        })
+        .unwrap_or_else(|_| factory(bits, version))
+}
@@ -16,0 +54,2 @@ pub struct Decoder {
+    #[cfg(has_asm)]
+    asm_isa: Option<u8>,
@@ -31,0 +71,2 @@ impl Decoder {
+            #[cfg(has_asm)]
+            asm_isa: None,
@@ -120 +161,7 @@ impl Decoder {
-            .and_then(|factory| factory(instruction_bits, self.version))
+            .and_then(|factory| {
+                #[cfg(has_asm)]
+                if let Some(isa) = self.asm_isa {
+                    return asm_instruction(instruction_bits, isa, self.version, factory);
+                }
+                factory(instruction_bits, self.version)
+            })
@@ -878,0 +926,9 @@ pub fn build_decoder<R: Register>(isa: u8, version: u32) -> Decoder {
+#[cfg(has_asm)]
+pub(crate) fn build_asm_decoder(isa: u8, version: u32) -> Decoder {
+    let mut decoder = build_decoder::<u64>(isa, version);
+    if version <= VERSION2 {
+        decoder.asm_isa = Some(isa);
+    }
+    decoder
+}
+
@@ -904,0 +961,32 @@ fn standard_factory<R: Register, const B: bool, const A: bool>(
+
+#[cfg(all(test, has_asm))]
+mod tests {
+    use super::*;
+    use crate::machine::{VERSION0, VERSION1};
+
+    #[test]
+    fn assembly_factory_cache_matches_uncached_decoding_across_modes() {
+        let mut words: Vec<_> = (0..=u32::from(u16::MAX)).collect();
+        let mut bits = 0x1234_5678u32;
+        for _ in 0..16_384 {
+            bits = bits.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
+            words.push(bits | 3);
+        }
+        let modes: Vec<_> = [VERSION0, VERSION1, VERSION2]
+            .into_iter()
+            .flat_map(|version| {
+                [0, ISA_A, ISA_B, ISA_A | ISA_B].map(|isa| {
+                    let decoder = build_asm_decoder(isa, version);
+                    (isa, version, decoder.standard_factory.unwrap())
+                })
+            })
+            .collect();
+        for bits in words {
+            for &(isa, version, factory) in &modes {
+                let expected = factory(bits, version);
+                assert_eq!(asm_instruction(bits, isa, version, factory), expected);
+                assert_eq!(asm_instruction(bits, isa, version, factory), expected);
+            }
+        }
+    }
+}
diff --git a/vendor/ckb-vm/src/machine/asm/mod.rs b/vendor/ckb-vm/src/machine/asm/mod.rs
index 09a3c73..16e1803 100644
--- a/vendor/ckb-vm/src/machine/asm/mod.rs
+++ b/vendor/ckb-vm/src/machine/asm/mod.rs
@@ -21 +21 @@ use crate::{
-    decoder::{Decoder, build_decoder},
+    decoder::{Decoder, build_asm_decoder},
@@ -564 +564 @@ where
-        let mut decoder = build_decoder::<u64>(self.machine.isa(), self.machine.version());
+        let mut decoder = build_asm_decoder(self.machine.isa(), self.machine.version());
```

</details>

## Round 2 · Dense RV64 compressed-instruction tables

Discarded. A read-only table for each supported VM version covered the entire 16-bit compressed instruction space. The assembly decoder used one directly indexed load on compressed factory misses. ISA extensions still used their existing wide-instruction factories. Public generic decoders, memory permissions, macro-op fusion and guest metering used the original rules.

The same four-replica mixed fixture ran uninstrumented in A–B–B–A order. Every run acknowledged and confirmed 55,000 transactions. The independent verifier replayed 1,043 blocks and 220,000 signed transactions. Actual in-window offers were 2,193–2,200/s.

| Order | Common TPS | Write-to-common p99 | Validator CPU | Mean admission check | Mean Follower prepare |
| --- | ---: | ---: | ---: | ---: | ---: |
| Exact `main` A1 | 2,116.84 | 1.409 s | 95.90 s | 309.20 μs | 87.81 ms |
| Compressed table B1 | 2,080.80 | 1.731 s | 99.88 s | 323.86 μs | 100.78 ms |
| Compressed table B2 | 2,109.72 | 1.681 s | 98.00 s | 317.32 μs | 96.41 ms |
| Exact `main` A2 | 2,131.16 | 1.435 s | 95.35 s | 308.31 μs | 87.87 ms |

Average CPU rose 3.5%. p99 rose 22.9% and 17.1%, and Follower preparation slowed in both pairings. Throughput was lower in both directions. Table footprint, cache locality and the added lookup path are possible causes. This experiment did not collect CPU cache events, and one isolated process could behave differently from four validators. Keeping this table would add memory without a demonstrated benefit.

The vendored assembly suite passed 168 tests. The focused comparison checked all 65,536 words for each of the three supported VM versions, including reserved encodings. The locked release workspace passed 529 tests with 29 ignored. The candidate SHA-256 was `fd3f1694beeb1c0daf56f05dc1daa1a7deb0c572376722f8d8b9bb6132b44ea8`. Apply the patch to the same pinned `main` revision and repeat the four mixed profiles to reproduce.

<details>
<summary>Rejected compressed-instruction table patch</summary>

```diff
diff --git a/vendor/ckb-vm/src/decoder.rs b/vendor/ckb-vm/src/decoder.rs
index 4b0a49b..6529422 100644
--- a/vendor/ckb-vm/src/decoder.rs
+++ b/vendor/ckb-vm/src/decoder.rs
@@ -2,0 +3,2 @@ use ckb_vm_definitions::registers::{RA, ZERO};
+#[cfg(has_asm)]
+use std::sync::OnceLock;
@@ -868,0 +871,13 @@ pub fn build_decoder<R: Register>(isa: u8, version: u32) -> Decoder {
+    build_standard_decoder::<R, false>(isa, version)
+}
+
+#[cfg(has_asm)]
+pub(crate) fn build_asm_decoder(isa: u8, version: u32) -> Decoder {
+    if version <= VERSION2 {
+        build_standard_decoder::<u64, true>(isa, version)
+    } else {
+        build_decoder::<u64>(isa, version)
+    }
+}
+
+fn build_standard_decoder<R: Register, const RVC_TABLE: bool>(isa: u8, version: u32) -> Decoder {
@@ -871,4 +886,4 @@ pub fn build_decoder<R: Register>(isa: u8, version: u32) -> Decoder {
-        (false, false) => standard_factory::<R, false, false>,
-        (true, false) => standard_factory::<R, true, false>,
-        (false, true) => standard_factory::<R, false, true>,
-        (true, true) => standard_factory::<R, true, true>,
+        (false, false) => standard_factory::<R, false, false, RVC_TABLE>,
+        (true, false) => standard_factory::<R, true, false, RVC_TABLE>,
+        (false, true) => standard_factory::<R, false, true, RVC_TABLE>,
+        (true, true) => standard_factory::<R, true, true, RVC_TABLE>,
@@ -879 +894 @@ pub fn build_decoder<R: Register>(isa: u8, version: u32) -> Decoder {
-fn standard_factory<R: Register, const B: bool, const A: bool>(
+fn standard_factory<R: Register, const B: bool, const A: bool, const RVC_TABLE: bool>(
@@ -885,0 +901,4 @@ fn standard_factory<R: Register, const B: bool, const A: bool>(
+        #[cfg(has_asm)]
+        if RVC_TABLE {
+            return asm_compressed_instruction(bits as u16, version);
+        }
@@ -904,0 +924,27 @@ fn standard_factory<R: Register, const B: bool, const A: bool>(
+
+#[cfg(has_asm)]
+fn asm_compressed_instruction(bits: u16, version: u32) -> Option<Instruction> {
+    static DECODED: [OnceLock<Box<[Instruction]>>; VERSION2 as usize + 1] =
+        [const { OnceLock::new() }; VERSION2 as usize + 1];
+    let table = DECODED[version as usize].get_or_init(|| {
+        (0..=u32::from(u16::MAX))
+            .map(|word| rvc::factory::<u64>(word, version).unwrap_or(0))
+            .collect()
+    });
+    // Valid results encode the instruction length and are therefore nonzero.
+    let instruction = table[usize::from(bits)];
+    (instruction != 0).then_some(instruction)
+}
+
+#[cfg(all(test, has_asm))]
+#[test]
+fn compressed_assembly_tables_match_the_factory() {
+    for version in 0..=VERSION2 {
+        for bits in 0..=u16::MAX {
+            assert_eq!(
+                asm_compressed_instruction(bits, version),
+                rvc::factory::<u64>(u32::from(bits), version),
+            );
+        }
+    }
+}
diff --git a/vendor/ckb-vm/src/machine/asm/mod.rs b/vendor/ckb-vm/src/machine/asm/mod.rs
index 09a3c73..16e1803 100644
--- a/vendor/ckb-vm/src/machine/asm/mod.rs
+++ b/vendor/ckb-vm/src/machine/asm/mod.rs
@@ -21 +21 @@ use crate::{
-    decoder::{Decoder, build_decoder},
+    decoder::{Decoder, build_asm_decoder},
@@ -564 +564 @@ where
-        let mut decoder = build_decoder::<u64>(self.machine.isa(), self.machine.version());
+        let mut decoder = build_asm_decoder(self.machine.isa(), self.machine.version());
```

</details>

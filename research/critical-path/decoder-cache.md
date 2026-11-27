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

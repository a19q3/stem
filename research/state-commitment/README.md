# Native Cell commitment on full payment blocks

Compare worker widths, multi-level Patricia nodes in RocksDB, an authenticated decoded-node cache and Spora MuHash on the same mature Stem Cell state. The worker comparison prepares signed blocks through the full Follower path. The commitment comparison starts with the exact sorted Cell mutations produced by that preparation.

## Follower worker scaling

The fixture reuses the signed 2,048-payment candidates and the committed height 1,148 archive from the [Follower preparation study](../follower-preparation/README.md). The locked `230a0e3` release build enabled `native-candidate-cell-prefetch`. Every process was pinned to the same eight physical cores `0–7` in the 96 MiB CCD of a Ryzen 9 7950X3D. The two RocksDB warmup blocks, archive state, cache capacity and transaction order were common. Worker widths were traversed in both directions. Reported medians pool 32 preparations per mode and width. A partial cache had 1,365 signature verification hits and 683 misses. A hot cache had 2,048 hits.

| Workers | Hot Follower wall | Hot Patricia wall | Hot process CPU | Partial Follower wall | Partial Patricia wall | Partial process CPU |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 2 | 108.63 ms | 72.13 ms | 173.34 ms | 175.40 ms | 71.65 ms | 265.95 ms |
| 4 | 75.51 ms | 41.34 ms | 191.97 ms | 108.05 ms | 40.01 ms | 284.21 ms |
| 8 | 57.98 ms | 25.85 ms | 217.27 ms | 76.42 ms | 24.52 ms | 316.98 ms |

Eight workers reduced hot preparation wall time by 46.6% relative to two and raised its process CPU by 25.3%. Partial preparation wall time dropped 56.4% with 19.2% more process CPU. All 192 preparations matched the prior independent one-worker Headers, ordered receipts and cycle charges. These are isolated Follower results. Four replicas sharing cores and RocksDB would need a matched network comparison before changing their worker defaults.

## Commitment comparison design

The mature snapshot contained 1,440,002 Cell leaves and 2,880,003 reachable nodes. Traversal authenticated every node against its content-addressed identity and the archived Cell root. Each timed proposal contributes 2,048 deletions and 2,048 additions. Every deletion was matched to the previous authenticated value. The sampled commitments share the same parent and signed blocks as the Follower comparison. An independent serial preparation also matched their complete Headers.

The readings begin at sorted `(Cell key, optional Cell value digest)` changes and include the Patricia traversal, canonical hashing and node journal creation. Cell-key preparation, RocksDB writes, WAL and VM validation are outside the timer. The equivalence checks compare the exact Patricia root, previous-value callbacks and byte-for-byte node put and delete journal against a serial oracle for each block. The balanced comparison rotates variants within each block and repeats in reverse worker order, with two disjoint warmup blocks. It creates a new candidate-local page cache for every mutation. One logical read means one RocksDB Get. Value bytes report payload returned by that API before decoding. Filesystem and physical-device reads can differ.

The packed databases store connected groups of two, three or four branching levels under their root node's content ID. Each page holds the compact node entries, with their individual content IDs and lengths. The normal traversal loads a page when it reaches the page root. The page reader places its remaining nodes in a sharded candidate-local map. Every node used by the traversal still passes the normal hash check. The matched fresh point control writes one canonical compact node per record using the same sorted build, RocksDB options, read options, statistics and 256 MiB block-cache capacity as the page stores. The archive control also reports the cost with the production-written SST layout. The page models read the immutable base. Updating the physical page layout is outside the timer.

The resident-byte variant keeps canonical nodes in memory and performs the ordinary validation on every load. The decoded-node variant builds its cache from the authenticated snapshot and feeds the already verified nodes back to the same mutation algorithm. It binds the lookup to this exact immutable state. A separate scheduling variant raises the subtree task budget to four times the actual Rayon worker count while retaining the default split threshold and canonical journal order.

## Matched state update costs

All wall times and CPU times below are medians over 32 updates at each width. The per-block RocksDB counters are invariant across widths for the reported variants.

| Source or algorithm | 2 workers | 4 workers | 8 workers | CPU at 8 workers | RocksDB Gets | Value bytes returned |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Archive, one node per record | 49.32 ms | 29.16 ms | 19.13 ms | 96.28 ms | 38,491 | 2.62 MB |
| Fresh, one node per record | 54.74 ms | 32.46 ms | 20.65 ms | 110.08 ms | 38,491 | 2.62 MB |
| Packed, 2 levels | 44.33 ms | 27.27 ms | 18.58 ms | 88.98 ms | 19,245 | 5.48 MB |
| Packed, 3 levels | 40.26 ms | 25.33 ms | 18.13 ms | 77.41 ms | 12,793 | 7.83 MB |
| Packed, 4 levels | 38.30 ms | 24.40 ms | 17.74 ms | 75.19 ms | 9,958 | 11.24 MB |
| Resident canonical bytes | 22.50 ms | 14.92 ms | 13.17 ms | 40.25 ms | 0 | 0 |
| Resident verified decoded nodes | 17.18 ms | 12.11 ms | 11.39 ms | 29.78 ms | 0 | 0 |
| Archive, fourfold subtree task budget | 58.86 ms | 34.70 ms | 22.60 ms | 119.24 ms | 38,491 | 2.62 MB |

**Packing preserved the commitment and paid off under the matched fresh-store control.** At two workers the four-level reader was 30.0% faster than fresh point reads and 22.4% faster than the archive. At eight workers the gains narrowed to 14.1% and 7.3%, respectively. Four levels reduced logical Gets by 74.1% and increased returned payload by 4.3×. Three levels used 30.4% fewer returned bytes than four, at an additional 0.39 ms at eight workers. For the full snapshot, logical key and value bytes summed to 290.78 MB for fresh point records, 334.00 MB for two levels, 319.91 MB for three and 315.63 MB for four. These are warm, immutable-base measurements. Page maintenance after insertions, deletions and branch collapse, compaction and cold-device behavior remain unmeasured.

The snapshot-wide decoded cache took 1.24 s to build after the authenticated snapshot read. Its hash table used an estimated 477 MB in key and value slots, plus bucket control bytes. Comparing it with resident canonical bytes isolates the benefit of avoiding repeat hash checks and node decoding in this fixture. Its steady-state update saved 1.78 ms at eight workers. This benchmark leaves the hit rate and maintenance cost of a bounded, exact-view cache under real block churn open. Increasing the task budget alone regressed at every tested width. All 768 matched preparations produced the same roots, previous-value callbacks and byte-for-byte node journals as serial, including the resident readers and the over-partitioned variant.

## Spora MuHash ablation

The standalone release comparison links Spora's `spora-muhash` at `949280fe313832c79dcf87305a1d0838b69f39a5`, with versions inherited from its Cargo lockfile. It uses the implementation's 3072-bit accumulator and native element mapping. The input for one element is the 32-byte Stem state key followed by the 32-byte canonical value digest. Start each proposal from the same serialized and reloaded parent accumulator. Fold 128 mutations per Rayon task, combine the task products and finalize one digest. [Spora implementation](https://github.com/a19q3/Spora/blob/949280fe313832c79dcf87305a1d0838b69f39a5/crypto/muhash/src/lib.rs)

| Workers | Wall including finalization | Process CPU |
| ---: | ---: | ---: |
| 2 | 4.057 ms | 7.970 ms |
| 4 | 2.106 ms | 8.149 ms |
| 8 | 1.086 ms | 8.400 ms |

Seeding the accumulator from 1,440,002 already materialized elements took 345 ms at eight workers. All 96 timed roots matched an independent serial delta fold. Applying each inverse delta restored the original accumulator. Two sampled results also matched independent rebuilds from the complete edited live set. The timed fold assumes the old digest of each spent Cell is already available. In the fixture, a separate serial pass to retrieve and recheck 2,048 old values took a median 13.66 ms. That pass models a cost if deletion inputs are fetched again.

MuHash measures multiset commitment arithmetic over prepared mutations. Stem's existing inclusion and absence proofs, Patricia node reclamation and CKB verification depend on the authenticated tree and its root. A parallel MuHash maintained alongside that tree would still incur Patricia work. The experiment keeps MuHash off the protocol path. The Packed Patricia reads preserve the existing root and journal and establish a useful read-side result. Page writes and recovery determine whether that result transfers to a lasting storage layout.

## Reproduction anchors

- Signed corpus SHA-256 `63f3cd67ce0ebf9ab4a2f75919e3cfac4a3894770f58f3c0d0c7dd51bc6ea3fb`. Recreate the mature archive and corpus as specified in the [Follower preparation report](../follower-preparation/README.md#reproduction-and-correctness).
- Parent Cell root `b20355260db53ff7d11fa9471b3f58cd60554517668732eea0dddec949340c8f`. Traverse the authenticated tree left to right and emit the 64-byte key and value pairs. The snapshot input SHA-256 was `997c6ec324bb90742f6160c93c921f3e4c1976463b141b93055751c80c5e2275`.
- Generate the ordered deltas from corpus groups 1–18, with group indices starting at zero. The first two groups are warmups. For the measured groups use the parent timestamp plus 300 ms and the zero-based measured sample index. Emit a u32 little-endian mutation count followed by sorted entries of one operation byte and 64 element bytes. Use `0` for a deletion with its authenticated old value and `1` for an addition with its new value. The delta SHA-256 was `a0141d42cff33c9e0e7358d95370f5f9e3beb612afb527f1f4c2e96f7b85a194`.
- Build each layout from the same authenticated base, sort pages by their root ID, write without a WAL during the offline materialization, and wait for RocksDB flush before timing. Iterate worker widths `2,4,8,8,4,2` on CPU IDs `0–7`. Use an independent serial mutation oracle on each delta. Only the experimental results are retained in the repository.

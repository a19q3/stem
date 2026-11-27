# Spore settlement regression baseline and extension coverage

The ordinary route is pinned to Stem revision `07dd3e5c8fa60adf1325672bc630284fc7565164`. It uses the released CKB Spore V2 contract and preserves the native title and CKB Type established for each asset. For ordinary Cluster members, first mint uses the collection version and the owner's authorization. The later wallet and bridge transfers operate on the issued member. Inbound CKB observations in these devnets use the Stem guardian configuration. [Released-contract devnets and dependency history](upstream/collections/README.md) · [Guardian authority](../../OPERATIONS.md#validator-admission-and-guardian-authority)

| Regression case | Evidence checked by the wallet and WASM replay |
| --- | --- |
| Ordinary CKB-origin Cluster member | Import, original release, collection ownership change during custody and reimport. The wallet authenticates the member and collection histories at the same CKB anchor. |
| Ordinary Stem-origin Cluster member | Native issuance, CKB mint, wallet transfer, native restoration and release of the same CKB Type on re-export. The wallet pins the exact collection output used at issuance. |
| Ordinary Stem-origin unclustered Spore | CKB wallet melt, authenticated native retirement, disappearance of the custodial title and inclusion of the refund. |
| Ordinary member with two CKB custody grants | Two native handoffs using CKB custody changes, followed by terminal melt, native retirement and refund. |
| Immortal member | Live re-export and both custody grants. The devnet report retains the rejected melt attempts. |

Run the pinned corpus with the release wallet and the built WASM package from the revision being considered. The executable checks the CKB histories and confirmed Stem receipts in the base and issuance-dependency archives. The base archive also contains a registered-member control. The archived CKB proofs use the recorded CKB Headers. The WASM replay verifies native proofs against the pinned genesis committees. Use the funded development-chain commands for a fresh contract execution and fresh deployment. [Baseline proof archive](upstream/collections/settlements.json.xz) · [Optimized issuance archive](upstream/collections/issuance-deps/settlements.json.xz) · [Live settlement procedure](../../OPERATIONS.md#exercise-ckb-spore-settlement)

```sh
research/spore-permit/replay-baseline.sh target/release/stem "$sdk_directory"
```

## Standard deposit preflight

Run the standard new-deposit check before signing either chain's custody transaction. Pass the complete Molecule SporeData. For a member, resolve its Cluster ID to the current live CKB collection under the released Cluster Type. Export that Cell's exact Type and raw data to JSON as `{"type": {...}, "data": "0x..."}`. Supply the file with `--cluster`. Unclustered objects omit that option.

```sh
stem intercell spore-payload --deposit --original-type "$type_hash" \
  --recipient "$recipient" --data sporedata.mol --cluster collection.json
```

The preflight rejects member-level executable extensions on the standard route. It also rejects absent or mismatched collection data, unrecognized collection layouts and populated collection-level Mutants. Resolve the live Cell on a trusted CKB endpoint and match it to the collection actually used by the draft. The command validates the supplied bytes before building a payload. Deployments with dedicated verified extension routes still need their pinned policy and lifecycle checks. Existing custodial assets use their configured release or recovery paths. The preflight is a wallet and operator control. A manually constructed CKB transaction can still place an output under a deployed escrow Lock.

Agent-only issuance is deferred. Operators require the collection owner's authorization when constructing a new standard mint. Already issued ordinary members remain eligible under the deposit checks above. Use the historical wallet verifier when deployment policy requires the exact mint authority or registration.

## Extension compatibility

| Profile | Evidence established | Further acceptance |
| --- | --- | --- |
| Immortal, approvals and large content | The devnet suites cover rejected holder melts, the specified approval Lock and the tested content sizes. The largest 496 KiB runs use beta.2 registered Cluster members in both origin directions. [Approval evidence](../../evidence/2026-11-27-approved-standard-spore.json.xz) · [496 KiB runs](upstream/beta2/handoff.md) | Each additional Lock and wallet route needs its own compatibility evidence. |
| One or two member-level Lua Mutants | Both origins ran with the published interpreter. Verified cases include immutable constraints, lock changes, custody grants and distinct program fee recipients. [Released Lua profiles](upstream/beta2/README.md) · [Fees and registration](upstream/beta2/registered-policy/README.md) · [SDK policies](upstream/beta2/sdk-policies/README.md) | Establish lifecycle and authorization equivalence for each new program. |
| Lua rules bound to transaction position | Local runs cover specific output positions and original-contract rejections of incompatible custody layouts. [SDK policy and position trials](upstream/beta2/sdk-policies/README.md) | Provide an accepted custody layout and execute each supported path with the original policy. |
| Proxy and Agent delegation (Agent-only mint deferred) | Four public mints spent and renewed an Agent, cited the exact Cluster version and spent another input under the Cluster holder's Lock. The wallet can prove each role. [Witnessed public mint histories](upstream/collections/agent/README.md) | A mint authorized solely by the Agent still needs a positive sample and a devnet lifecycle. |
| Collection-level Lua Mutant (deferred) | Indexed releases and devnet attempts have bounded evidence. The archived extended collection records carry no Mutant reference. The tested populated collection was rejected at creation. [Collection data shapes](upstream/collections/README.md#public-deployment-and-collection-data) · [Published beta.2 creation attempt](upstream/beta2/README.md#cluster-membership-and-collection-level-registration) | Authenticate a confirmed issuance with a populated field, its registration and program. Then verify member issuance, wallet transfer, custody and return. |
| Generic L1 historical authorization in StemVM | The wallet can verify exact consumed inputs, dependencies and intermediate states under a caller-trusted CKB anchor. [Wallet interface](../../OPERATIONS.md#offline-history-of-a-ckb-type) | A protocol path for contract callers needs its own proof format or compatible ABI, anchor provenance, resource bounds and verification across native replicas. |

Item 20 is **Core Complete / Extended Compatibility Deferred**. Freeze the supported V2 settlement behavior and keep the verified Mutant profiles in this matrix. Continue auditing bridge custody, refund accounting and recovery. Spore V3 follows its own protocol and migration work.

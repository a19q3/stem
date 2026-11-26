# Offline CKB Type history

A wallet can verify the intermediate states of a CKB asset from a caller-selected birth OutPoint through a named transaction. The caller pins the CKB anchor and complete Type Script hash. Each transaction on the path must preserve a single predecessor and successor for this Type until a possible terminal burn. The verifier authenticates the raw transaction and its witnesses, follows the exact consumed OutPoint, and resolves the other inputs and all Cell Deps, including DepGroup members. External transactions supply hash-checked raw preimages. Only the transactions on the asset path need transaction-and-witness inclusion proofs. [Wallet verifier](../../src/ckb_wallet/type_history.rs) · [CKB Merkle verification](../../src/l1_proof.rs)

An optional ordered list of intermediate Lock or Cell-data hashes lets a caller require that particular states occurred before settlement. A state must satisfy every field specified in its predicate. Each predicate consumes one distinct intermediate output. The verifier reports the actual matching OutPoints. A complete bundle also contains the transactions, witnesses and dependency data for other offline checks.

| Archived case | CKB transactions | Verified outcome |
| --- | ---: | --- |
| Stem-born approved Spore | 4 | The object passed the required custody Lock, then returned to the protected holder. |
| CKB-born immortal Cluster member | 4 | The original object entered custody, returned and entered custody again. |
| Stem-born immortal Spore | 4 | The ownership path reached the second export. |
| Stem-born 496 KiB Spore | 5 | The native object went through custody and the last CKB transaction burned the standard representation. |
| Mutable mailbox | 4 | Four distinct Cell-data hashes. Two intermediate states occurred in the requested order. |
| Revisable Metatype workflow | 5 | The full history includes an ownership-only transition, two witnessed revisions and the burn. The intermediate OutPoints and complete path match the existing workflow-specific recovery. |
| Fresh approved Spore | 4 | A new four-replica run verified the integrated wallet commands at agreed Stem height 39. |

The unmodified shared-Manifest workflow and live-ceiling recovery fixtures also completed on fresh four-replica networks at heights 35 and 69. The archived reports include the actual escrow and CKB transactions, the existing recovery outputs and their controls. [Complete portable witnesses](histories.json.xz) · [Four-replica and CKB reports](integration.json.xz)

Build the release CLI and run the following command without an RPC endpoint.

```sh
python3 research/type-history/replay.py --stem target/release/stem
```

The replay checks seven histories and fifteen rejection cases. It verifies failures for a changed witness, swapped inclusion, skipped Header, a suppressed middle transition even when its authentic raw transaction is reattached as a reference, and reversed, repeated or endpoint-only state requirements. [Replay program](replay.py) · [Exact results](replay-results.json)

The caller selects and authenticates the CKB anchor independently. A proof stops at the named end transaction. An output shown there may have been spent later. Global uniqueness depends on the deployed Type rules. The verifier proves what occurred on CKB. Enforcing an arbitrary CKB program during a later Stem transfer still requires a consensus rule for that transfer. [CLI and input format](../../OPERATIONS.md#offline-history-of-a-ckb-type)

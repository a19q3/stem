# Stem experiment archive

Each experiment round has a durable report and its available run evidence.
The report names the baseline, tested revision, workload, hardware and
toolchains, commands, comparison conditions, results and run-specific limits.
Keep negative results and superseded approaches alongside successful runs.
Add new dated records for reruns so that original measurements remain
available. Published network evidence contains public genesis and proof data.

- [Current Stem measurements](../BENCHMARKS.md)
- [Earlier Fern measurements](../FERN-BENCHMARKS.md)
- [Research scope and current verification status](../STEM-EVOLUTION.md)

| Recent round | Report and evidence |
| --- | --- |
| Registered Lineage Heads and paired authorization | [Report](../BENCHMARKS.md#paired-authorization-and-lineage-lifecycle), [evidence](2026-11-25-stem-lineage.json) |
| Dynamic Lineage, conflicting execution and AMM | [Report](../BENCHMARKS.md#dynamic-lineage-commands), [evidence](2026-11-25-stem-dynamic-lineage.json) |
| Portable and reused assembly Executor comparison | [Report](../BENCHMARKS.md#executor-backend-selection), [evidence](2026-11-25-stem-executor-backends.json) |
| Repeated native signature recovery | [Report](../BENCHMARKS.md#repeated-native-signature-recovery), [evidence](2026-11-25-stem-secp-reuse.json) |
| Native Keeper | [Report](../BENCHMARKS.md#native-keeper), [evidence](2026-11-25-stem-keeper.json.xz) |
| Independent bilateral Lineage authorization and Keeper settlement | [Report](../BENCHMARKS.md#sponsored-bilateral-lineage-settlement), [evidence](2026-11-25-stem-sponsored-lineage.json.xz) |
| Direct CKB finality and scaled committee verification | [Report](../BENCHMARKS.md#direct-ckb-finality), [initial rounds](2026-11-25-stem-direct-finality.json.xz), [pagination rerun](2026-11-25-stem-direct-finality-pagination.json.xz) |
| Direct CKB and xUDT withdrawals | [Report](../BENCHMARKS.md#direct-bridge-withdrawals), [evidence](2026-11-25-stem-direct-payout.json.xz) |
| CKB-funded committee change regression | [Report](../BENCHMARKS.md#ckb-funded-committee-regression), [evidence](2026-11-25-stem-bridge-funded-stake-rerun.json.xz) |
| Direct CKB stake retirement, delayed withdrawal and slashing | [Report](../BENCHMARKS.md#direct-ckb-stake-retirement), [evidence](2026-11-25-stem-direct-retirement.json.xz) |
| Native weighted Jolteon and frozen CometBFT comparison | [Report](../BENCHMARKS.md#native-weighted-jolteon), [successful and failed rounds](2026-11-25-stem-jolteon-runtime.json.xz) |
| Frozen CometBFT 128-replica completion after cleanup | [Report](../BENCHMARKS.md#native-weighted-jolteon), [raw records](2026-11-25-stem-comet-128-post-clean.json.xz) |
| Weighted Jolteon restart after clean rebuild | [Report](../BENCHMARKS.md#native-weighted-jolteon), [signed proofs and replica data](2026-11-25-stem-jolteon-post-clean.json) |
| Native Jolteon proof verification in CKB-VM | [Report](../BENCHMARKS.md#jolteon-verifier-in-ckb-vm), [raw cycles and fingerprints](2026-11-25-stem-jolteon-ckb-vm.json) |
| CKB-funded Jolteon handoffs and native CKB checkpoints | [Report](../BENCHMARKS.md#ckb-funded-jolteon-handoffs), [successful and failed runs](2026-11-25-stem-jolteon-handoffs.json.xz) |
| Jolteon CKB and xUDT payouts, retirement and slashing | [Report](../BENCHMARKS.md#jolteon-withdrawals-retirement-and-slashing), [12 runs including the failed report attempt](2026-11-25-stem-jolteon-l1-consumers.json.xz) |
| Jolteon time, long-gap recovery, anchored snapshot and 128-member CKB relay | [Recovery report](../BENCHMARKS.md#jolteon-clock-and-long-gap-recovery), [relay report](../BENCHMARKS.md#128-member-checkpoint-through-ckb-p2p), [18 runs](2026-11-25-stem-clock-recovery-ckb-relay.json.xz) |
| Default Jolteon runner, ordinary retention and CKB regression | [Report](../BENCHMARKS.md#default-jolteon-runtime-and-ordinary-retention), [raw runs](2026-11-25-stem-default-jolteon.json.xz) |
| CKB fee fund settlement and bridge regression | [Report](../BENCHMARKS.md#ckb-fee-fund-settlement), [raw runs including the failed attempt](2026-11-25-stem-fee-funds.json.xz) |
| Fixed-precision resource pricing | [Report](../BENCHMARKS.md#fixed-precision-fee-calibration), [both attempts and CKB regression](2026-11-25-stem-resource-pricing.json.xz) |
| CKB delegation, retirement, slashing and capped Jolteon handoffs | [Report](../BENCHMARKS.md#ckb-delegation-and-weighted-handoffs), [all devnet runs and failure evidence](2026-11-25-stem-ckb-delegation.json.xz) |
| In-term CKB delegation reductions, historical penalties and L1 reorg | [Report](../BENCHMARKS.md#in-term-ckb-delegation-reductions), [raw runs and failed attempt](2026-11-25-stem-delegation-reduction.json.xz) |
| Compact delegation, individual liability and validator switching | [Report](../BENCHMARKS.md#compact-delegation-and-position-specific-liability), [raw runs and failed attempt](2026-11-25-stem-delegation-window.json.xz) |
| Historical stake principal, commission and closed-epoch fees | [Report](../BENCHMARKS.md#closed-epoch-stake-rewards-and-commission), [successful runs and build failures](2026-11-25-stem-stake-rewards.json.xz) |
| Individual reward redemption, replay protection and CKB reserve reconciliation | [Report](../BENCHMARKS.md#individual-stake-reward-redemption), [successful runs and failed attempts](2026-11-25-stem-stake-reward-claims.json.xz) |
| Accrued stake rewards, restart durability and CKB redemption | [Report](../BENCHMARKS.md#accrued-stake-reward-balances), [both reward rounds and bridge regressions](2026-11-25-stem-stake-reward-accrual.json.xz) |
| Shared checkpoint enforcement at genesis | [Report](../BENCHMARKS.md#shared-ckb-checkpoint-at-genesis), [substitution, recovery, bridge and build evidence](2026-11-25-stem-checkpoint-policy.json.xz) |
| Read-only CKB references in StemVM | [Report](../BENCHMARKS.md#ckb-cell-references-in-stemvm), [successful run, regressions and early failures](2026-11-26-stem-l1-references.json.xz) |
| Independent CKB anchor refresh | [Report](../BENCHMARKS.md#independent-ckb-anchor-refresh), [both refresh runs and bridge regression](2026-11-26-stem-anchor-refresh.json.xz) |
| Delegation occupancy and both vote heights | [Report](../BENCHMARKS.md#delegation-occupancy-and-both-vote-heights), [devnet runs and build evidence](2026-11-26-stem-delegation-liability.json.xz) |
| Bidirectional Intercell Messaging | [Report](../BENCHMARKS.md#bidirectional-intercell-messaging), [three message rounds, regressions and rejection evidence](2026-11-26-stem-intercell.json.xz) |
| Stem-native UDT export and return | [Report](../BENCHMARKS.md#stem-native-udt-settlement), [full and partial returns, rejections and regressions](2026-11-26-stem-udt-settlement.json.xz) |
| CKB Spore V2 settlement through Stem | [Report](../BENCHMARKS.md#ckb-spore-v2-settlement-through-stem), [three settlement rounds and regression evidence](2026-11-26-stem-spore-settlement.json.xz) |
| Stem native compound object settlement | [Report](../BENCHMARKS.md#native-compound-object-settlement-over-ckb), [three object runs, initial failure and regressions](2026-11-26-stem-object-settlement.json.xz) |
| Sparse CKB checkpoint publisher | [Report](../BENCHMARKS.md#sparse-ckb-checkpoint-publication), [successful and failed publisher and Intercell runs](2026-11-26-stem-checkpoint-publisher.json.xz) |
| Consensus-time Keeper scheduling | [Report](../BENCHMARKS.md#consensus-time-keeper), [raw samples, replica proofs and first failed measurement](2026-11-26-stem-keeper-time.json.xz) |
| L1 checkpoint expenses from the PoW Security Fund | [Report](../BENCHMARKS.md#ckb-checkpoint-fees-from-the-pow-security-fund), [paired estimates, canonical proofs, failed runs and regressions](2026-11-26-stem-checkpoint-fee-fund.json.xz) |
| Paid CKB application security domain | [Report](../BENCHMARKS.md#paid-ckb-application-security-domain), [raw service, penalty, exit and Stem receipt runs](2026-11-26-stem-security-domain.json.xz) |

| Intercell Mailbox recovery and canonical CKB replay state | [Report](../BENCHMARKS.md#intercell-mailbox-recovery-after-ckb-reorganization), [raw rounds, failures and regressions](2026-11-26-stem-intercell-recovery.json.xz) |

| Native OTX Group Actions and independent sponsorship | [Report](../BENCHMARKS.md#native-otx-action-authorization), [raw proofs, failures and Intercell regression](2026-11-26-stem-otx-actions.json.xz) |

| Atomic Lineage batch settlement with independent sponsors | [Report](../BENCHMARKS.md#atomic-lineage-batch-settlement), [three replica rounds, failures and raw proofs](2026-11-26-stem-lineage-batch.json.xz) |

| Keeper height and time scheduling for atomic Lineage batches | [Report](../BENCHMARKS.md#keeper-scheduling-for-atomic-lineage-batches), [both replica rounds, rollback cases and raw proofs](2026-11-26-stem-keeper-batch.json.xz) |

| Prepaid bounded Keeper retries | [Report](../BENCHMARKS.md#prepaid-bounded-keeper-retries), [both replica rounds, failures and authenticated retry state](2026-11-26-stem-keeper-retry.json.xz) |

| Sustained Keeper load and finalized admission expiry | [Report](../BENCHMARKS.md#sustained-keeper-load-and-admission-expiry), [4- and 16-replica traces, rejections and controls](2026-11-26-stem-keeper-load.json.xz) |

| Competing CKB checkpoint publishers and canonical adoption | [Report](../BENCHMARKS.md#ckb-checkpoint-publisher-handoff), [signed transactions, rejected probes and reruns](2026-11-26-stem-checkpoint-adoption.json.xz) |

| 128-member checkpoint contention across CKB peers | [Report](../BENCHMARKS.md#128-member-checkpoint-contention-on-ckb), [matrices, rejected spends, signed transactions and pool traces](2026-11-26-stem-ckb-checkpoint-contention.json.xz) |

| CKB-funded 128-member Jolteon handoff | [Report](../BENCHMARKS.md#ckb-funded-128-member-jolteon-handoff), [signed stakes, finality proofs, failure logs and regression](2026-11-26-stem-funded-wide-handoff.json.xz) |

| Funded 128-member boundary and successor over CKB P2P | [Report](../BENCHMARKS.md#128-member-epoch-boundary-relayed-over-ckb-p2p), [peer traces, committed cycles, exact transactions and failed attempts](2026-11-26-stem-wide-epoch-p2p.json.xz) |

| Automatic CKB checkpoint adoption and paged catch-up | [Report](../BENCHMARKS.md#automatic-ckb-checkpoint-successor-discovery), [transactions, journal outcomes, failures and regressions](2026-11-26-stem-publisher-auto-follow.json.xz) |
| Historical Intercell delivery after CKB checkpoint advancement | [Report](../BENCHMARKS.md#intercell-delivery-after-checkpoint-advancement), [paired costs, raw proofs, rejections and regressions](2026-11-26-stem-intercell-history.json.xz) |
| Portable CKB security-domain accounting | [Report](../BENCHMARKS.md#portable-ckb-security-domain-accounting), [raw evidence, rejected audits and regressions](2026-11-26-stem-security-domain-audit.json.xz) |
| Authenticated per-vault fee attribution | [Report](../BENCHMARKS.md#authenticated-per-vault-fee-attribution), [dual-vault and fee-fund proofs, regressions and failures](2026-11-26-stem-vault-revenue.json.xz) |
| Browser WASM transaction and proof SDK | [Report](../BENCHMARKS.md#browser-wasm-transaction-and-proof-sdk), [browser, npm and CKB-backed proof runs](2026-11-26-stem-wasm-sdk.json.xz) |
| Sustained native payments and durable write attribution | [Report](../BENCHMARKS.md#sustained-native-payments-and-durable-write-attribution), [signed workloads, timing samples and regression logs](2026-11-26-stem-storage-load.json.xz) |
| Intercell delivery with multiple CKB references | [Report](../BENCHMARKS.md#intercell-delivery-with-multiple-ckb-references), [signed transactions, reference certificates, proofs and failed runs](2026-11-26-stem-intercell-composition.json.xz) |
| Intercell delivery across funded committee handoffs | [Report](../BENCHMARKS.md#intercell-delivery-across-funded-committee-handoffs), [4-, 16- and 128-voter runs, signed transactions, P2P and rejection evidence](2026-11-26-stem-intercell-epoch.json.xz) |
| Sustained replicated Lineage and AMM contention | [Report](../BENCHMARKS.md#sustained-replicated-lineage-and-amm-contention), [signed workloads, timing, stress controls and failure logs](2026-11-26-stem-lineage-load.json.xz) |
| Lineage proposal selection at the read limit | [Report](../BENCHMARKS.md#proposal-selection-after-the-lineage-read-limit), [matched probes, network controls and build evidence](2026-11-26-stem-proposal-selection.json.xz) |
| Jolteon control transport under Lineage load | [Report](../BENCHMARKS.md#jolteon-control-transport-under-lineage-load), [pinned controls, pacemaker samples and recovery regressions](2026-11-26-stem-transport-priority.json.xz) |
| Bounded parallel Jolteon admission experiment | [Report](../BENCHMARKS.md#bounded-parallel-admission-experiment), [raw network runs, fixed-tip timings and production validation](2026-11-26-stem-admission-batching.json.xz) |
| Intercell sparse replay proofs and authenticated recovery | [Report](../BENCHMARKS.md#intercell-sparse-replay-proofs-and-authenticated-recovery), [successful devnets, failed control, proofs and commands](2026-11-26-stem-intercell-revalidation.json.xz) |
| Jolteon leader-directed transaction relay | [Report](../BENCHMARKS.md#jolteon-leader-directed-transaction-relay), [matched workloads, overload controls and fault evidence](2026-11-26-stem-jolteon-leader-relay.json.xz) |
| Sustained submission and admission latency | [Report](../BENCHMARKS.md#sustained-submission-and-admission-latency), [matched AMM controls, payments and per-stage timing](2026-11-26-stem-load-dispatch.json.xz) |
| Reuse of staged proposal execution | [Report](../BENCHMARKS.md#reuse-of-staged-proposal-execution), [exact Header comparison, network traces and regressions](2026-11-26-stem-proposal-reuse.json.xz) |
| Compact Patricia branch storage | [Report](../BENCHMARKS.md#compact-patricia-branch-storage), [paired timing, signed workloads, WAL counters and regressions](2026-11-26-stem-patricia-storage.json.xz) |
| Bounded submission connection reuse and CPU profile | [Report](../BENCHMARKS.md#bounded-submission-connection-reuse), [matched payments, CPU samples and regression evidence](2026-11-26-stem-submission-reuse.json.xz) |
| Standalone CKB Metatype composition | [Report](../BENCHMARKS.md#standalone-ckb-metatype-composition), [signed transactions, inclusion proofs and failed runs](2026-11-26-stem-metatype-ckb.json.xz) |
| Metatype object approvals and Polytype comparison | [Report](../BENCHMARKS.md#metatype-object-approvals-and-native-polytype-comparison), [signed cases, proof verification and unsuccessful runs](2026-11-26-stem-metatype-object.json.xz) |
| Immutable Metatype custody round trip | [Report](../BENCHMARKS.md#immutable-metatype-custody-round-trip), [signed cases, state proofs and failed attempts](2026-11-26-stem-metatype-custody.json.xz) |
| Sparse assembly trace invalidation | [Report](../BENCHMARKS.md#sparse-assembly-trace-invalidation), [paired payments, trace probe, CPU samples and regression logs](2026-11-26-stem-vm-trace-reset.json.xz) |
| RISC-V instruction-width dispatch | [Report](../BENCHMARKS.md#risc-v-instruction-width-dispatch), [ELF corpus, paired results, CPU samples and regression logs](2026-11-26-stem-vm-decoder.json.xz) |
| Shared Metatype manifest definitions | [Report](../BENCHMARKS.md#shared-metatype-manifest-definitions), [matched CKB layouts, custody proofs and failed build](2026-11-26-stem-metatype-shared.json.xz) |
| Cap-bound fungible Metatype custody | [Report](../BENCHMARKS.md#cap-bound-fungible-metatype-custody), [partial and full returns, matched controls, proofs and failed attempts](2026-11-26-stem-metatype-fungible.json.xz) |
| Borrowed Metatype Manifest views | [Report](../BENCHMARKS.md#borrowed-metatype-manifest-views), [eight paired CKB runs, proofs and failed start](2026-11-26-stem-metatype-view.json.xz) |
| Revisable Metatype objects across CKB | [Report](../BENCHMARKS.md#revisable-metatype-objects-across-ckb), [signed round trips, capacity boundary controls and failed runs](2026-11-26-stem-metatype-revision.json.xz) |
| VM verification reuse across parent changes | [Report](../BENCHMARKS.md#vm-verification-reuse-across-parent-changes), [paired payments, raw CPU profiles, CKB proofs and recovery](2026-11-26-stem-vm-parent-cache.json.xz) |
| Live Cell reuse across candidate branches | [Report](../BENCHMARKS.md#live-cell-reuse-across-candidate-branches), [matched payments, AMM replay and recovery evidence](2026-11-26-stem-live-cell-reuse.json.xz) |
| Deferred Live Cell record encoding | [Report](../BENCHMARKS.md#deferred-live-cell-record-encoding), [paired loads, write-order control and CKB proofs](2026-11-26-stem-live-cell-finality.json.xz) |
| Fixed-buffer Patricia reads | [Report](../BENCHMARKS.md#fixed-buffer-patricia-reads), [editor timings, signed loads and CKB proofs](2026-11-26-stem-patricia-inline-read.json.xz) |
| Reused RocksDB point-read options | [Report](../BENCHMARKS.md#reusing-rocksdb-point-read-options), [storage probes, signed loads and CKB proofs](2026-11-26-stem-patricia-point-reads.json.xz) |
| Serial admission dispatch | [Report](../BENCHMARKS.md#serial-admission-dispatch), [receipt controls, signed loads and CKB proofs](2026-11-26-stem-inline-admission.json.xz) |
| Stake Registry loading during admission | [Report](../BENCHMARKS.md#stake-registry-loading-during-admission), [funded states, signed reads and CKB proofs](2026-11-26-stem-staked-admission.json.xz) |
| Reusing decoded stake inputs across blocks | [Report](../BENCHMARKS.md#reusing-decoded-stake-inputs-across-blocks), [funded preparation controls and full-size CKB handoff](2026-11-26-stem-stake-view-cache.json.xz) |

Read compressed run records with `xz -dc benchmarks/<file>.json.xz`.

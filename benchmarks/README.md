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

Read compressed run records with `xz -dc benchmarks/<file>.json.xz`.

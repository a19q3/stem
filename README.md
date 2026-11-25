# STEM — One Cell. Many worlds.

Rooted in CKB.

From the community, by the community, for the community.

May the Cells grow.

Stem is a standalone Rust network with a Polytype Cell Model. A Cell has one Lock Script and any number of Type Scripts within protocol limits. Stem derives Script Groups from the consumed and created Cells and runs them in StemVM on its vendored CKB-VM backend. Weighted Jolteon orders transactions and certifies execution Headers.

Stem uses CKB capacity. A threshold-certified bridge backs spendable Capacity and wrapped xUDT with reserves on CKB L1. Validator power can come from a separate CKB Stake Registry, which verifies finalized Stem exclusion before starting the CKB unbonding period. New networks commit to their own identity and genesis.

The `stem run` command starts a Jolteon replica. Ordinary replicas retain a recent execution window, while archive replicas serve older payloads and proofs. An observer can restore from a state image pinned to a confirmed CKB checkpoint. The bridge guardians audit the confirmed CKB chain for mint and reconciliation. Anyone can relay a pending withdrawal proof to the vault's Jolteon checkpoint. Custody authorizes consolidation, reserve moves and rotation. Stake and custody have independent authorization.

Local runs reached 128 replicas. Four replicas tested recovery from a skewed leader clock and a validator missing more than 220 blocks. CKB devnet runs cover committee handoffs, permissionless CKB and xUDT withdrawals, retirement, delayed unbonding and vote penalties. A 128-member checkpoint passed CKB P2P relay and inclusion with default transaction pool settings. The CometBFT baseline is frozen at `12744bf` for reproducible comparisons. [Run and inspect the Jolteon experiments](OPERATIONS.md#exercise-the-native-jolteon-path)

CKB delegation adds individually locked principal to a validator's eligible voting weight. Each position occupies 154 CKB and has separately proved activation and exclusion heights. Delegators can reduce part or all of a position during the validator's term. CKB enforces an unbonding delay and can penalize double voting within the position's active interval. [CKB delegation exercise](OPERATIONS.md#exercise-ckb-delegation)

Lineage commands can authorize work on registered Cells by stable ID. Stem resolves the current Head, runs the signed RISC-V Executor, and validates all affected Script Groups before committing. A rejected operation can settle its pre-authorized fee while preserving every Head. The `stem wallet lineage` commands build and seal these transactions. A Keeper-enabled network can reserve a one-shot command and execute it at a committed block height. For a sponsored intent, business parties sign first and a separate payer can fund execution.

Bridged networks expose SystemRoot proofs at `/stem/bridge-backing`, `/stem/bridge-lane` and `/stem/bridge-withdrawal`. They authenticate Stem's accounting of certified CKB observations, including recognized reserve, circulating Capacity, pending withdrawals and lane nonces. The fee tally is available at `/stem/extinguished-fees`. Networks that configure fee funds expose `/stem/fee-funds` for the four accounts and support permissionless claims to their fixed CKB recipients.

- [Protocol](STEM-PROTOCOL.md)
- [Implementation and VM ABI](ARCHITECTURE.md)
- [Build, configuration and reproducible network tests](OPERATIONS.md)
- [Measurements and historical comparison](BENCHMARKS.md)
- [Experiment reports and raw evidence](benchmarks/README.md)
- [Protocol evolution and research status](STEM-EVOLUTION.md)

The protocol needs an independent review before custody of real funds. See the implementation status in the [protocol](STEM-PROTOCOL.md#9-当前实现与实测证据).

# STEM — One Cell. Many worlds.

Rooted in CKB.

From the community, by the community, for the community.

May the Cells grow.

Stem is a standalone Rust network with a Polytype Cell Model. A Cell has one Lock Script and any number of Type Scripts within protocol limits. Stem derives Script Groups from the consumed and created Cells and runs them in StemVM on its vendored CKB-VM backend. Weighted Jolteon orders transactions and certifies execution Headers.

Stem uses CKB capacity. A threshold-certified bridge backs spendable Capacity and wrapped xUDT with reserves on CKB L1. Validator power can come from a separate CKB Stake Registry, which verifies finalized Stem exclusion before starting the CKB unbonding period. New networks commit to their own identity and genesis.

The `stem run` command starts a Jolteon replica. Ordinary replicas retain a recent execution window, while archive replicas serve older payloads and proofs. An observer can restore from a state image pinned to a confirmed CKB checkpoint. Genesis binds bridge vaults and stake retirement to that same checkpoint. The bridge guardians audit the confirmed CKB chain for mint and reconciliation. Anyone can relay a pending withdrawal proof. The checkpoint publisher supports scheduled, on-demand and epoch handoff updates with durable progress. A CKB Lock can fund eligible checkpoint fees from the PoW Security Fund. Custody authorizes consolidation, reserve moves and rotation. Stake and custody have independent authorization. [Run the publisher](OPERATIONS.md#run-a-sparse-ckb-checkpoint-publisher)

Local runs reached 128 replicas. Four replicas tested recovery from a skewed leader clock and a validator missing more than 220 blocks. CKB devnet runs cover committee handoffs, permissionless CKB and xUDT withdrawals, retirement, delayed unbonding and vote penalties. A 128-member checkpoint passed CKB P2P relay and inclusion with default transaction pool settings. The CometBFT baseline is frozen at `12744bf` for reproducible comparisons. [Run and inspect the Jolteon experiments](OPERATIONS.md#exercise-the-native-jolteon-path)

CKB delegation adds individually locked principal to a validator's eligible voting weight. Each position occupies 146 CKB and has separately proved activation and exclusion heights. Delegators can reduce part or all of a position during the validator's term. CKB enforces an unbonding delay and can penalize double voting within the position's active interval. Closed-epoch budgets and individual shares can be verified against a signed Header. Anyone can relay rewards to the beneficiary's authenticated balance and redeem them to the same CKB Lock once they meet its minimum output capacity. [CKB delegation and revenue exercises](OPERATIONS.md#exercise-ckb-delegation)

Lineage commands can authorize work on registered Cells by stable ID. Stem resolves the current Head, runs the signed RISC-V Executor, and validates all affected Script Groups before committing. A rejected operation can settle its pre-authorized fee while preserving every Head. The `stem wallet lineage` commands build and seal these transactions. Independent Lineage intents can share an atomic plan with separate sponsors. A Keeper-enabled network can reserve a one-shot command or atomic batch and execute it by height or finalized consensus time, with an optional prepaid retry policy. For a sponsored intent, business parties sign first and a separate payer can fund execution.

Bridged networks expose SystemRoot proofs at `/stem/bridge-backing`, `/stem/bridge-lane` and `/stem/bridge-withdrawal`. They authenticate Stem's accounting of certified CKB observations, including recognized reserve, circulating Capacity, pending withdrawals and lane nonces. StemVM contracts can read guardian-certified historical CKB outputs and live-at-anchor observations. The fee tally is available at `/stem/extinguished-fees`. Networks that configure fee funds expose `/stem/fee-funds` for the four accounts and `/stem/stake-reward-claim` for position-level claims. [CKB Cell reference workflow](OPERATIONS.md#read-a-ckb-cell-from-stemvm)

Applications can fund independent CKB security domains for paid Live Cell attestations. The service emits typed receipts and enforces objective penalties and delayed release on CKB. Stem applications can authenticate those receipts through the L1 reference interface. A portable audit verifies capital and service payouts through a selected CKB transition. [Run a security domain](OPERATIONS.md#exercise-a-paid-ckb-security-domain) · [Verify its accounts](OPERATIONS.md#verify-security-domain-capital-and-income)

Configured networks carry Intercell messages between Stem and a CKB Mailbox. The native checkpoint verifies delivery on CKB, and guardians certify the CKB output used for delivery on Stem. Both chains enforce replay protection. A relay can rebuild its CKB inbox index from the live Mailbox. A Stem application can escrow its native UDT, issue a CKB representation, and redeem the underlying asset after a confirmed burn. The basic Spore V2 route restores the original CKB Spore after ownership moves on Stem. A native Stem object can also travel in the opposite direction with its transfer approval enforced on both chains. [Intercell workflow](OPERATIONS.md#exercise-intercell-messaging)

- [Protocol](STEM-PROTOCOL.md)
- [Implementation and VM ABI](ARCHITECTURE.md)
- [Build, configuration and reproducible network tests](OPERATIONS.md)
- [Measurements and historical comparison](BENCHMARKS.md)
- [Experiment reports and raw evidence](benchmarks/README.md)
- [Protocol evolution and research status](STEM-EVOLUTION.md)

The protocol needs an independent review before custody of real funds. See the implementation status in the [protocol](STEM-PROTOCOL.md#9-当前实现与实测证据).

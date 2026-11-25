# STEM — One Cell. Many worlds.

Rooted in CKB.

From the community, by the community, for the community.

May the Cells grow.

Stem is a standalone Rust network with a Polytype Cell Model. A Cell has one Lock Script and any number of Type Scripts within protocol limits. Stem derives the required Script Groups from the consumed and created Cells, and runs them in StemVM on its vendored CKB-VM backend. CometBFT orders transactions and certifies the application state.

Stem uses CKB capacity. A threshold-certified bridge backs spendable Capacity and wrapped xUDT with reserves on CKB L1. Validator power can come from a separate CKB Stake Registry. New networks commit to their own identity and genesis.

The `stem run` executable uses the Strand ledger, group-indexed messages and Witnesses, authenticated RocksDB state and state sync. Devnet runs cover four-validator finality and restart, remote state sync, bidirectional CKB and xUDT, L1-derived validator addition and removal, and Checkpoint Type ID publications on CKB. The deployment assumes that the bridge guardians audit the confirmed CKB chain and that custodians release funds against verified Stem receipts. Stake and custody have independent authorization.

Bridged networks expose SystemRoot proofs at `/stem/bridge-backing` and `/stem/bridge-lane`. They authenticate Stem's accounting of certified CKB observations, including recognized reserve, circulating Capacity, pending withdrawals and lane nonces. Development-network fee accounting is available separately at `/stem/extinguished-fees`.

- [Protocol](STEM-PROTOCOL.md)
- [Implementation and VM ABI](ARCHITECTURE.md)
- [Build, configuration and reproducible network tests](OPERATIONS.md)
- [Measurements and historical comparison](BENCHMARKS.md)
- [Protocol evolution and research status](STEM-EVOLUTION.md)

The protocol needs an independent review before custody of real funds. See the implementation status in the [protocol](STEM-PROTOCOL.md#9-当前实现与实测证据).

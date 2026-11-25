# Fern

Fern is a standalone Rust network for composable Cells. A Cell has one Lock Script and any number of Type Scripts within protocol limits. Fern derives the required Script Groups from the consumed and created Cells, and runs them in Fern VM on its vendored CKB-VM backend. CometBFT orders transactions and certifies the application state.

Fern uses CKB capacity. A threshold-certified bridge backs spendable Capacity and wrapped xUDT with reserves on CKB L1. Validator power can come from a separate CKB Stake Registry. New networks commit to their own identity and genesis.

The `fern run` executable uses the native ledger, group-indexed Witnesses, authenticated RocksDB state and state sync. Devnet runs cover four-validator finality and restart, remote state sync, bidirectional CKB and xUDT, L1-derived validator addition and removal, and Checkpoint Type ID publications on CKB. The deployment assumes that the bridge guardians audit the confirmed CKB chain and that custodians release funds against verified Fern receipts. Stake and custody have independent authorization.

- [Protocol](FERN-PROTOCOL.md)
- [Implementation and VM ABI](ARCHITECTURE.md)
- [Build, configuration and reproducible network tests](OPERATIONS.md)
- [Measurements and historical comparison](BENCHMARKS.md)

The deployment still has finite L1 Registry lifetime limits, and the protocol needs an independent review before custody of real funds. See the implementation status in the [protocol](FERN-PROTOCOL.md#9-当前实现与实测证据).

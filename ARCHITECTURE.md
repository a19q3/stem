# Fern native ledger and CKB staking

The native ledger crate, its CKB-VM host, and the Stake Registry contract can be built and tested in this workspace. The current `fern run` entry point still uses the CKB transaction format, existing RocksDB state, and its configured CometBFT committee. The native ledger, snapshot certificates, and Merkle commitments have yet to be connected to ABCI finalization, state sync, wallets, and the bridge. Use a separate genesis for that cutover.

## Native ledger

[Fern's Molecule schema](ledger/schemas/ledger.mol) encodes a Cell as a table containing Capacity, a CKB Lock Script, a vector of CKB Type Scripts, and Cell Data. The raw Transaction contains Cell Deps, header deps, inputs with Since, and Cells. The witnessed Transaction adds the witness vector. The native decoder rejects the original CKB transaction format.

The Type Script set is sorted by the bytes of the full canonical Script, including code hash, hash type, and args. Constructors sort the set. Decoders require strict ascending order. The limits are 32 Type Scripts, 16 KiB per Script, and 64 KiB of Type Script bytes per Cell. Ordinary transactions are limited to 512 KiB. A standalone Cell can be up to 2 MiB so genesis can hold the 1 MiB secp256k1 data Cell. Resolution bounds both the number and total bytes of references, including members of a Dep Group.

Minimum Capacity in shannons is 100,000,000 multiplied by the exact encoded Cell length. The length includes 20 bytes for the table size and field offsets, 8 for Capacity, the complete Lock Script, 4 bytes for the Type Script vector size, 4 bytes of offset plus the complete encoding of each Type Script, and 4 bytes of length plus all Cell Data. Every output and every decoded live Cell satisfies this minimum. An ordinary transaction consumes the checked difference between total input and output Capacity as a fee. Bridge reserve accounting remains in the current node implementation.

Resolution derives the Lock Script Groups from input Cells and the Type Script Groups from both sides. Each group runs once with the ordered input and output indexes where its Script occurs. A Script used in both roles has two independent groups. Cell Deps must be live, and a transaction cannot consume its own Cell Dep or a Dep Group member. Type-hash code lookup indexes every Type Script on each resolved code Cell and rejects matches to different binaries. Since uses height, epoch fraction, or milliseconds from the supplied block context and committed input origins.

The network ID and separate domain strings bind raw transaction IDs, witnessed transaction commitments, and OutPoint-bound Live Cell commitments. The secp256k1 lock hashes the network-bound transaction ID in its normal sighash witness layout. The native Merkle trees commit the network ID, leaf count, and witnessed transaction or full Live Cell leaf. Live Cells are ordered by canonical OutPoint bytes. The VM cache binds the witnessed transaction, resolved Cell bytes and origins, and declared headers. It charges recorded cycles on a hit. The Rust proof APIs verify inclusion relative to a supplied root. The existing Go verifier and the node do not yet supply or authenticate native roots.

## CKB-VM host

The native host fixes CKB-VM to version 2 and reuses the vendored process scheduler and the assembly allocation pool where available. Spawn, Exec, and their children see the same script group and immutable data. Serial and parallel group execution share one transaction cycle budget. Parallel batches divide the remaining budget for speculation and retry a cycle-limited group against the remaining budget in deterministic order.

Calls retain the CKB buffer convention. `a0` is the buffer pointer, `a1` points to its length, and `a2` is the byte offset. Cell sources in `a4` are Input 1, Output 2, Cell Dep 3, Group Input `0x0100000000000001`, and Group Output `0x0100000000000002`. `a3` selects the Cell or group index. Errors use CKB's 1 for an out-of-range index and 2 for an absent item.

| Call | Result |
| --- | --- |
| 2051 and 2061 | Full native Transaction and its network-bound ID |
| 2052 and 2062 | Current group's complete Script and Script hash |
| 2071 | Full native Cell, including Type Scripts and Cell Data |
| 2081 | Capacity 0, Data hash 1, Lock Script 2, Lock hash 3, occupied Capacity 6 |
| 2110 | Canonically encoded full Type Script vector |
| 2111 | Type Script at the index in `a5` |
| 2112 | Type Script hash at the index in `a5` |
| 2113 | Complete matching Type Script, with `a5` pointing to a 32-byte Script hash |
| 2114 | One byte for group role (0 Lock, 1 Type), then little-endian u32 input and output counts |
| 2115 | Absolute little-endian u32 transaction index for the group-relative index |
| 2116 | Fern network ID |

The native host uses the existing witness, Cell Data, input, header, and process call numbers. A creation header is visible when named by a header dep. Fern headers carry no extension data. Fields 4 and 5 under 2081 have no native value. Contracts discover the Type Script set via 2110–2113.

The fixture suite runs independently compiled RISC-V Asset, Collateral, Transfer Policy, Identity, Lifecycle, and Settlement contracts. It signs the transactions with the CKB secp256k1 implementation and checks value conservation, input-side constraint removal, identity forgery, witness tampering, and signature replay across network IDs.

## CKB Stake Registry

The [Stake Registry contract](contracts/src/bin/stake-registry.rs) runs on CKB itself. The Registry is a unique Type ID Cell authenticated by the first input and output index. A Stake Cell's Type Script binds the Registry hash and one Ed25519 validator key. Its Capacity must equal native CKB L1 occupied Capacity plus the declared bond. The contract forces a paired Registry update, checks the exact Stake Cell contents and bond, and rejects duplicate registrations. The Registry stores records in public-key order with matching Stake Cell commitments. Withdrawal leaves a permanent tombstone for the key.

Registration requires the validator's Ed25519 signature and an input authorized by the Stake Cell owner. The owner can increase a live bond or request exit. Activation and retirement certificates use separate domains and pin the guardian set in the Registry. Retirement can take effect after the configured epoch boundary and requires a signer quorum attesting actual exclusion from the CometBFT committee. The new retired Stake Cell must outlive the configured relative CKB block delay before withdrawal. Registry guardian rotation requires a quorum of both the departing and arriving authorities over the same state transition.

The contract verifies both signatures over CometBFT 0.38 canonical vote sign bytes. Chain ID, key, height, round, vote type, distinct Block IDs, and the authenticated liability interval determine admissibility. The evidence key has one value for the vote position regardless of vote ordering. A proved equivocation forfeits the full positive bond as CKB transaction fee and records the penalty while preserving the remaining Cell and its retirement liability. No production parameters are set here. The current fixed Registry limit is 256 public keys including tombstones.

The guardian observer checks all live Stake Cells against the confirmed CKB Registry and persists its last L1 block anchor locally. `fern stake verify` requires strictly more than two-thirds of pinned guardian weight for the same snapshot and derives power by integer division. Eligible stakes are ordered by decreasing bond then key, limited to the configured committee size, and checked against the configured total and CometBFT bounds. The first epoch starts at height 1. Subsequent epochs start at `1 + epoch * blocks_per_epoch`, with the proposed ABCI update at that height minus two.

This is guardian authentication of a trusted CKB full node's state. Guardians also have to authenticate activation and retirement from actual finalized Fern committee history. The L1 contract enforces their authorization and the delayed spend. The CometBFT node still needs the staking-driven genesis, snapshot admission and H+2 update hooks, handoff verification, and authenticated retirement publisher. Stake Cells and the bridge vault use separate L1 scripts and state.

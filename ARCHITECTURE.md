# Fern native ledger and CKB staking

The native application has a four-node CometBFT integration test and a synchronous RocksDB commit path. The default `fern run` entry point still uses the existing bridge, wallet and committee stack. The native Stake Registry can initialize genesis from authenticated L1 observations. Full staking handoffs, the native bridge and transport for state sync remain integration work.

## Native ledger

[Fern's Molecule schema](ledger/schemas/ledger.mol) encodes a Cell as a table containing Capacity, a CKB Lock Script, a vector of CKB Type Scripts, and Cell Data. The raw Transaction contains Cell Deps, header deps, inputs with Since, and Cells. The witnessed Transaction adds a vector of group-addressed proofs and seals and an optional operation certificate. The native decoder rejects the original CKB transaction format.

The Type Script set is sorted by the bytes of the full canonical Script, including code hash, hash type, and args. Constructors sort the set. Decoders require strict ascending order. The limits are 32 Type Scripts, 16 KiB per Script, and 64 KiB of Type Script bytes per Cell. Ordinary transactions are limited to 512 KiB. A standalone Cell can be up to 2 MiB so genesis can hold the 1 MiB secp256k1 data Cell. Resolution bounds both the number and total bytes of references, including members of a Dep Group.

The network-configured Storage Bond is the sum of canonical Cell bytes and fixed state overhead, multiplied by shannons per byte. The encoded Cell includes the table's 20-byte prefix, its 8-byte Capacity field, the complete Lock Script, the Type Script vector size and entries, and the length and bytes of Cell Data. Ordinary transactions consume the checked difference between input and output Capacity as fees. The native store commits cumulative extinguished Capacity under the SystemRoot.

Every transaction derives its Lock Groups from the consumed Cells and its Type Groups from the union of input and output Type Scripts. Each complete Script runs once per role and can read the entire transaction. Cell Deps must be live. Resolved code lookup rejects ambiguous Type Script matches with different code bytes. Time locks reference candidate height or preceding committed time in milliseconds. Relative locks also read the authenticated Cell creation metadata.

Transactions use canonical group numbers sorted by role and complete Script bytes. Sparse Witness entries carry a 16-bit group index, proof data and a seal. The native secp256k1 Lock Script verifies a digest over network, raw transaction ID, complete Lock Script hash, role and a commitment to all nonempty proof data. The transaction root commits full Witness bytes. The VM cache commits all witnessed bytes, resolved Cells, origins and declared Headers and charges measured cycles on a hit.

The Cell and system roots use the same incremental Patricia implementation. Each state record appears once, with its hash in the leaf. An MMR commits preceding native Header IDs and stores its leaf count and peaks on the application commit path. Separate ordered Merkle trees authenticate transactions and receipts. A single synchronous RocksDB batch writes affected nodes, records, execution, the Header and the MMR frontier. State snapshots contain canonical records and the retained Header suffix. Installation requires an independently authenticated application hash and reconstructs both state roots.

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
| 2074, 2117, 2118 | Full group witness, proof bytes, seal bytes. Source 0 and index 0 select the current group. Source 1 selects the absolute group at index `a3` |
| 2119 | Standard authorization digest for the current Lock Group |
| 2072, 2082 | Native Header, or fields height 0, time 1, parent ID 2, transaction root 3, Cell root 4, system root 5, history root 6, receipt root 7 |

The native host uses the existing Cell Data, input, Header and process call numbers. Call 2083 field 1 returns the nine-byte native Since. Spawn and Exec can read group proof bytes using the absolute group index as the witness data index. A creation header is visible when named by a header dep. Fern headers carry no extension data. Fields 4 and 5 under 2081 have no native value. Contracts discover the Type Script set via 2110–2113.

The fixture suite runs independently compiled RISC-V Asset, Collateral, Transfer Policy, Identity, Lifecycle, and Settlement contracts. It signs the transactions with the CKB secp256k1 implementation and checks value conservation, input-side constraint removal, identity forgery, witness tampering, and signature replay across network IDs.

## CKB Stake Registry

The [Stake Registry contract](contracts/src/bin/stake-registry.rs) runs on CKB itself. The Registry is a unique Type ID Cell authenticated by the first input and output index. A Stake Cell's Type Script binds the Registry hash and one Ed25519 validator key. Its Capacity must equal native CKB L1 occupied Capacity plus the declared bond. The contract forces a paired Registry update, checks the exact Stake Cell contents and bond, and rejects duplicate registrations. The Registry stores records in public-key order with matching Stake Cell commitments. Withdrawal leaves a permanent tombstone for the key.

Registration requires the validator's Ed25519 signature and an input authorized by the Stake Cell owner. The owner can increase a live bond or request exit. Activation and retirement certificates use separate domains and pin the guardian set in the Registry. Retirement can take effect after the configured epoch boundary and requires a signer quorum attesting actual exclusion from the CometBFT committee. The new retired Stake Cell must outlive the configured relative CKB block delay before withdrawal. Registry guardian rotation requires a quorum of both the departing and arriving authorities over the same state transition.

The contract verifies both signatures over CometBFT 0.38 canonical vote sign bytes. Chain ID, key, height, round, vote type, distinct Block IDs, and the authenticated liability interval determine admissibility. The evidence key has one value for the vote position regardless of vote ordering. A proved equivocation forfeits the full positive bond as CKB transaction fee and records the penalty while preserving the remaining Cell and its retirement liability. No production parameters are set here. The current fixed Registry limit is 256 public keys including tombstones.

The guardian observer checks all live Stake Cells against the confirmed CKB Registry and persists its last L1 block anchor locally. `fern stake verify` requires strictly more than two-thirds of pinned guardian weight for the same snapshot and derives power by integer division. Eligible stakes are ordered by decreasing bond then key, limited to the configured committee size, and checked against the configured total and CometBFT bounds. The first epoch starts at height 1. Subsequent epochs start at `1 + epoch * blocks_per_epoch`, with the proposed ABCI update at that height minus two.

This is guardian authentication of a trusted CKB full node's state. Guardians also have to authenticate activation and retirement from actual finalized Fern committee history. The L1 contract enforces their authorization and the delayed spend. The native application accepts an authenticated genesis committee, admits snapshots and emits updates for H+2 activation. The default executable still needs the native cutover. Handoff verification and the authenticated retirement publisher remain open. Stake Cells and the bridge vault use separate L1 scripts and state.

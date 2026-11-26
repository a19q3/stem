# Stem native ledger and CKB staking

The default `stem run` command executes the native state machine under weighted Jolteon. It commits current state, application receipts, MMR peaks, finality evidence and the consensus journal together in RocksDB. The native bridge and stake observers authenticate confirmed CKB data outside the consensus process.

Block preparation sorts changed record hashes per Patricia namespace, then passes each subtree a contiguous range of changed keys. Shared paths are loaded and rebuilt once for the block. The same edit removes superseded nodes. Snapshot restoration builds both trees from ordered records in batches and checks the roots before the atomic write.

## Polytype Cell Model

[Stem's Strand schema](ledger/schemas/strand.json) encodes the Cell with inline scripts, an optional dense Type Script vector and application Data. The raw transaction contains inputs, dependencies, Cells, Group Messages and optional protocol or expiry fields. The outer transaction contains proof data, seals and any certificate. Script identity keeps the complete CKB Script hash. CKB L1 wire types still use Molecule. The indexed and historical Molecule layouts are measured by the encoding suite.

The Type Script set is sorted by the full Strand encoding of each Script, including code hash, hash type, and args. Constructors sort the set. Decoders require strict ascending order. The limits are 32 Type Scripts, 16 KiB per Script, and 64 KiB of Type Script bytes per Cell. Ordinary transactions are limited to 512 KiB. A standalone Cell has a 2 MiB limit, which permits larger genesis allocations. Resolution bounds both the number and total bytes of references, including members of a Dep Group.

The network-configured Storage Bond is the sum of canonical Cell bytes and fixed state overhead, multiplied by shannons per byte. The Cell encodes one presence byte, eight Capacity bytes, the complete Lock Script, and four Data length bytes plus the payload. A nonempty Type Script vector contributes four count bytes and the full encodings of its Scripts. Each Script uses 37 bytes plus its arguments. Ordinary transactions consume the checked difference between input and output Capacity as fees. Genesis rates in millionths of a shannon let the Store combine byte, cycle and state costs before settling in whole shannons. The native store commits cumulative extinguished Capacity under the SystemRoot.

Every transaction derives its Lock Groups from the consumed Cells and its Type Groups from the union of input and output Type Scripts. Each complete Script runs once per role and can read the entire transaction. Cell Deps must be live. Resolved code lookup rejects ambiguous Type Script matches with different code bytes. Time locks reference candidate height or preceding committed time in milliseconds. Relative locks also read the authenticated Cell creation metadata.

Transactions assign group indices by role and the full packed CKB Script bytes. Sparse Witness entries carry a 16-bit group index, proof data and a seal. The standard secp256k1 Lock Script checks a digest binding the network, raw transaction ID, Lock role and full Script hash, resolved input Cells, explicit Cell Deps and expanded Dep Group members, full declared Header Deps, and all nonempty proof data. The raw ID includes Group Messages. Each group can add its seal independently, and the transaction root commits the full Witness. The VM cache also binds Cell origins, declared Headers and verified L1 references. A verification that reads the candidate height or committed parent records the parent Header ID. A verification with no such reads can be reused across parents after reference resolution and transaction time checks. Cache hits charge the measured cycles. Independently, each VM worker can reuse a successful secp256k1 recovery for the exact 65-byte seal and 32-byte digest. Its bounded cache only returns the recovered key. Each invocation still charges the fixed syscall cycles and the Lock Script checks the key.

The Cell and system roots use the same incremental Patricia implementation. Each state record appears once, with its hash in the leaf. An MMR commits preceding native Header IDs and stores its leaf count and peaks on the application commit path. Separate ordered Merkle trees authenticate transactions and receipts. A single synchronous RocksDB batch writes affected nodes, records, execution, the Header and the MMR frontier. State snapshots contain canonical records and the retained Header suffix. Installation requires an independently authenticated application hash and reconstructs both state roots.

RocksDB stores each Patricia branch with its bit length, the prefix
bytes through that bit length, and two child hashes. Loading a node
restores the canonical bytes in a fixed-size buffer before checking
the node hash. The editor borrows nodes it has already staged and
copies node bytes into an owned proof only when a proof is requested.
The local storage marker binds the stored layout to the genesis ID.
The committed Store and its candidate views share immutable default
RocksDB point-read options. Each lookup checks the candidate batches
first and reads the database on a miss.

A prepared block retains its decoded Live Cell changes by OutPoint.
The Patricia leaf hashes the OutPoint, creation metadata and Cell
bytes as one canonical record. Preparation feeds those slices
directly into the hasher. Finality encodes each created record with
one reused buffer and adds it to the atomic RocksDB batch in
the original write order. Candidate reads search changes newest first, and a consumed entry
blocks a read from the committed store. All branch views of
the same Store share a bounded cache of committed records, including
their authenticated creation metadata. Successful finalization
invalidates exactly the changed OutPoints. Snapshot installation
clears the cache after the atomic write.

### Independently signed pairs and sponsorship

The RISC-V Intent Lock uses the same recoverable signature syscall as the regular Lock. It signs one exact input, the exact delivered Cell, ordered resolved dependencies, expiry and a fee ceiling. The input's absolute position selects the delivery output at execution. Dependency resolution shares validation with complete signatures. Each paired signer commits to the same ordered Action manifest. The wallet and ledger identify a manifest entry by Lock or Type role and the full Script hash. Assembly maps stable identities to current group indices, and resolution hashes the actual messages the VM will deliver. A sponsor uses input and output 0 and signs the beneficiary pair at position 1. Paying at most the actual transaction fee is checked against resolved Capacity. The wallet derives group indices at assembly and attaches all partial proof data before complete transaction signers seal their groups.

### Lineage registry

An opt-in network pins the Data hash of the Lineage Type Script in genesis. The marker uses a stable LineageId and leaves Cell Data untouched, so another Type Script can keep its existing Data layout. Each verified lifecycle transaction updates the actual UTXO and a compact SystemRoot record in the same batch. The record names the current head OutPoint, version and Executor content hash. Registration reads an explicitly consumed anchor and verifies the Executor against resolved Cell Deps. Replacement and retirement require the current head's Lock and every applicable Type Group. The authenticated lookup returns the registry path and the current Live Cell path under one Header. Retirement returns an absence proof. The registration anchor prevents reuse without a permanent tombstone.

The same Strand transaction can carry a bounded Lineage command with an exact plain-CKB fee input and stable Lineage IDs. The Store resolves IDs in its candidate overlay and verifies the fee lock against a dedicated authorization digest. The registered RISC-V Executor reads only the command payload, declared Heads and immutable context. It supplies Data and Capacity for one successor per target. The standard resolve, capacity, Lock and Type Group checks run on the materialized Cells. Lock and Type Scripts stay fixed, and aggregate Head Capacity cannot change. The kernel uses the signed command ID for actual OutPoints and records the original transaction in the transaction tree. The receipt tree authenticates the fee, cycles and application outcome. Both the Cell and System trees update in the existing atomic RocksDB batch.

Conflicting commands resolve sequentially. A bounded wave can parallelize preflight and Executor work for commands with disjoint fee inputs, current Heads and potential output OutPoints, and only direct, read-only dependencies. The kernel rechecks the full materialized transaction and charges it in canonical order. Executor rejection and materialized script rejection discard every Head effect. The already authorized CKB refund path can consume the fee subject to resource limits. Read accounting includes the preflight and any validated attempt. The success path currently evaluates the fee Lock during preflight and during full verification.

The validation VM exposes authorization through syscall 2125 as one role byte plus the 32-byte digest. Role 0 identifies the single fee input. Role 1 is available only for Lock Groups consisting solely of resolved Heads. A mixed group receives no authorization. Ordinary groups retain the existing complete-transaction digest. The command syscall 2129 provides the original witnessed Strand envelope. During command validation, load-transaction exposes the actual materialized Cells and load-tx-hash returns the stable command ID used for output OutPoints.

The Executor uses the V2 instruction schedule and a bounded portable RISC-V VM. It meters ELF loading, instructions and copied bytes. Syscall 2126 with source 0 and index 0 loads height, parent time in milliseconds, payer Lock hash (zero for sponsored intents) and target count (8+8+32+4 bytes). Source 1 loads a target's LineageId, current version and Capacity (32+8+8 bytes). Syscall 2127 exposes the signed payload at source 0 and the indexed target's Data at source 1. Reads use the standard buffer, size and offset registers. Syscall 2128 takes Data pointer, Data length, target index and Capacity in a0 through a3. Exactly one output per target is required. The Executor cannot read the store or make outbound calls. [Fuel access lists](https://docs.fuel.network/docs/specs/protocol/tx-validity/) [Sui shared-object inputs](https://www.docs.sui.io/develop/transactions/ptbs/inputs-and-results)

### Lineage intent authorization

A Strand Lineage intent embeds an ordered actor set and fee cap in its raw transaction. Each actor signs the same canonical body with the fee input removed and exact fee normalized to zero. The sponsor then binds the actual fee OutPoint, selected amount, resolved fee Cell and complete actor seals. The fee seal and raw intent use separate signature domains. The business seals are included in the transaction commitment. A single exact target version is sufficient to prevent executing the same signed business effects twice because an applied operation advances every selected Head.

The common Lineage authorization preflight recovers the actor keys, verifies the fee Lock and charges recovery cycles before executing application code. This path also authorizes Keeper registration and validates again at wake time. Syscall 2126 source 2 exposes one verified 32-byte actor ID at the chosen index. Source 0 masks the payer field for sponsored intents so the Executor cannot vary its output based on the subsequently chosen fee Lock. The example bilateral Executor compares the two actor IDs with the two stored reserve owners before proposing a two-asset settlement.

A Lineage batch carries two to eight fully signed commands. Every business signature binds a network-scoped ordered plan, and each fee Lock authorizes its own payer and amount. The plan hashes normalized member terms without circular commitments to sponsor selection or the plan ID itself. The consensus entry point compares the plan to all submitted bindings before executing any member.

The Store stages each member through the existing Lineage transaction path. A journal records the previous values of affected Live Cell and Lineage overlay entries. A failed member restores those entries in reverse order and leaves the initial block budget intact. Members share a transaction cycle ceiling. The first member covers the batch envelope's extra byte charge. Finality commits one batch and one aggregate receipt. Individual output OutPoints derive from each signed member, and the batch inclusion proof authenticates those members.

### Keeper scheduling

The Keeper uses height buckets and an ordered timed index in SystemRoot. Individual Lineage commands and atomic batches use the same queue and execution path. All batch members bind the same trigger into their signed plan. The timed entry uses the earliest member expiry. The first member's fee OutPoint determines queue order and names the stored body. Every payer has an exclusive index entry binding that OutPoint, the trigger and the entire scheduled transaction ID. Cancellation consumes any one reserved fee Cell with its normal authorization and clears all entries for that exact ID.

The Store selects work from the parent Header and state before user transactions. Timed expiry is processed in height order, followed by ready entries ordered by target time and primary fee OutPoint. One selected command or batch uses a single block-derived resource share, which includes reservation reads and cleanup for every sponsor. Registration authenticates each sponsor and all business signatures and bounds their aggregate cycles. Wakeup uses the existing Lineage RISC-V materializer. Batch failure rolls back every member and emits a skip without charging an execution fee. State read faults abort block preparation.

A signed retry policy prepays two to four reservation shares and specifies the minimum block interval. Retry jobs reuse the timed agenda and its block slots. Failed atomic attempts keep the same command body and sponsor locks, updating only the authenticated eligible height and remaining attempts. The journal restores the candidate changes for a standalone retry job as well as for an atomic batch. A spent fee input, the signed expiry or exhausted attempt count ends the job. The initial trigger, member terms and every sponsor authorization stay fixed across all attempts.

Execution and receipt trees commit height attempts, time attempts and submitted transactions in that order. The queue, timed index, bodies and fee links support authenticated inclusion and absence. Snapshots include every pending reservation. The same Jolteon finality proof authenticates their SystemRoot and eventual execution receipt.


Each replica's admission pool indexes exclusive transaction height
deadlines, Keeper registration heights and Keeper registration times.
After a finalized commit it removes entries whose deadlines preclude
inclusion in the next block. Uncertified proposals and temporary queue
saturation leave entries eligible for later selection. Committed
reservations follow the Keeper state machine.

The proposer walks borrowed mempool entries in order and copies them
into the candidate list upon selection. It skips an entry when its
bytes or minimum Lineage reads exceed the remaining block budget.
The read floor covers the Executor context and both resolutions of the
fee input and declared dependencies. It also applies to a fee-only
outcome, and sums across members of an atomic batch. Candidate execution
and proposal verification still charge the exact resources.

The Jolteon proposer can retain the overlay and receipts from selection
for ordinary transactions and single Lineage commands without Header
Deps. It computes the final transaction root once selection ends and
rebases the new Live Cell origins before writing the authenticated
state batch. A script in that path sees the candidate height and the
committed parent. It cannot load a Header through the origin of a Cell.
An inspected Header Dep, a protocol operation, an atomic Lineage batch
or Keeper work uses full proposal preparation with the final context.
Other voters replay the selected transactions against the signed Header.

Transaction decoding checks output Scripts directly in the verified
Strand byte ranges. It enforces the packed Script length limit and the
canonical Type Script order before constructing any Cell used during
execution. Input, Cell Dep and Header Dep collections with multiple
entries go through the duplicate scan.

## CKB-VM host

The [StemVM crate](vm) owns the native ABI, verification cache and Group execution. Its RISC-V backend derives from CKB-VM v0.24.15 at `7199b96d9f24b51c4c5eba358d64cd4d9bcf8150`, with upstream Git history and license retained. Stem uses a fixed VM 2 configuration and reuses the process scheduler and assembly allocation pool. A host-only bitmap records populated instruction traces. Reset and pool reuse invalidate their lengths before loading different code. Installing a trace overwrites its entire payload, and both assembly backends check for a nonzero length before dispatch. After fetching instruction bytes with executable-page checks, the standard decoder dispatches by RISC-V instruction width and uses the configured ISA extensions. On a built-in miss, it tries user-registered factories in insertion order. Spawn and Exec see the same read-only Script Group context. [Upstream source](https://github.com/nervosnetwork/ckb-vm/tree/v0.24.15)

The block verifier resolves transactions and stages their state transitions in proposal order. Its single bounded Rayon pool runs verified contexts in waves, sharing workers across transactions and using idle slots for Script Groups. A local bounded LRU estimates Group costs from previously successful runs. Estimation changes only the initial local budget used by speculative execution. A Group that hits it is retried against the exact remaining budget in canonical order. The verifier charges every observed cycle in transaction order and discards the full block overlay if a transaction or block limit fails.

Calls retain the CKB buffer convention. `a0` is the buffer pointer, `a1` points to its length, and `a2` is the byte offset. Cell sources in `a4` are Input 1, Output 2, Cell Dep 3, Group Input `0x0100000000000001`, and Group Output `0x0100000000000002`. `a3` selects the Cell or group index. Errors use CKB's 1 for an out-of-range index and 2 for an absent item.

| Call | Result |
| --- | --- |
| 2051 and 2061 | Full native Transaction and its network-bound ID |
| 2052 and 2062 | Current group's complete Script and Script hash |
| 2071 | Full native Cell, including Type Scripts and Cell Data |
| 2081 | Capacity 0, Data hash 1, Lock Script 2, Lock hash 3, occupied Capacity 6, canonical Cell content hash 7 |
| 2110 | Canonically encoded full Type Script vector |
| 2111 | Type Script at the index in `a5` |
| 2112 | Type Script hash at the index in `a5` |
| 2113 | Complete matching Type Script, with `a5` pointing to a 32-byte Script hash |
| 2114 | One byte for group role (0 Lock, 1 Type), then little-endian u32 input and output counts |
| 2115 | Absolute little-endian u32 transaction index for the group-relative index |
| 2116 | Stem network ID |
| 2074, 2117, 2118 | Full group witness, proof bytes, seal bytes. Source 0 and index 0 select the current group. Source 1 selects the absolute group at index `a3` |
| 2119 | Standard authorization digest for the current Lock Group |
| 2121 | Recover a 33-byte compressed secp256k1 key from a 65-byte compact seal at `a0` and a 32-byte digest at `a1`. Writes the key at `a2`. Returns 4 for an invalid or high-S seal. Each call charges 1,900,000 cycles, including invalid seals. |
| 2122 | Candidate height and committed parent time as two little-endian u64 values, followed by the 32-byte parent Header ID. Source and index must be zero. |
| 2123 | The encoded Group Message. Source 0 and index 0 select the current group. Source 1 selects the absolute group index. |
| 2130 | Authenticated read-only CKB reference in OutPoint order within the verified reference set at index `a3`, with `a4=0`. Selector `a5=0` returns the full record. Selector `a5=1` returns the 32-byte CKB Type Script hash followed by Cell Data for a typed `LiveAtAnchor` record. A historical or untyped record returns `ITEM_MISSING` for selector 1. Both selectors use the normal buffer and offset ABI. |
| 2124 | For paired Lock authorization, the 32-byte network-bound ordered dependency digest, little-endian u64 transaction fee and the 32-byte Action manifest digest. Source and index must be zero. Available to the Data-addressed paired Lock code when the transaction uses the paired proof scope. |
| 2072, 2082 | Native Header, or fields height 0, time 1, parent ID 2, transaction root 3, Cell root 4, system root 5, history root 6, receipt root 7 |

The native host uses the existing Cell Data, input, Header and process call numbers. Call 2083 field 1 returns the nine-byte native Since. Spawn and Exec can read group proof bytes using the absolute group index as the witness data index. A creation header is visible when named by a header dep. Header source 5 and index 0 read the committed parent directly. Both source 5 and syscall 2122 record a parent read for the VM cache. Spawn and Exec share the read flag with the root VM. The resolved candidate height is derived from the parent height. Stem headers carry no extension data. Fields 4 and 5 under 2081 have no native value. Contracts discover the Type Script set via 2110–2113.

The fixture suite runs independently compiled RISC-V Asset, Collateral, Transfer Policy, Identity, Lifecycle, and Settlement contracts. It signs the transactions with the CKB secp256k1 implementation and checks value conservation, input-side constraint removal, identity forgery, witness tampering, and signature replay across network IDs.

## CKB Stake Registry

The [Stake Registry contract](contracts/src/bin/stake-registry.rs) runs on CKB itself. The Registry is a unique Type ID Cell authenticated by the first input and output index. A Stake Cell's Type Script binds the Registry hash and one Ed25519 validator key. Its Capacity must equal native CKB L1 occupied Capacity plus the declared bond. The contract forces a paired Registry update, checks the exact Stake Cell contents and bond, and rejects duplicate registrations. The Registry stores records in public-key order with matching Stake Cell commitments. Withdrawal removes the current record and inserts the key into a permanent sparse Merkle commitment. Registration verifies non-inclusion under the current root. This frees the active slot and permanently retires the old public key.

Registration requires the validator's Ed25519 signature and an input authorized by the Stake Cell owner. The signature binds the initial Registry record, including a fixed commission rate from 0 to 10,000 basis points. The owner can increase a live bond or request exit. The Registry's current guardian set authorizes activation. An L1 retirement consumes the Registry and Stake Cell and reads the Registry's pinned direct checkpoint. Its SystemRoot must contain the validator's permanent retirement marker at or before the authenticated Stem height. The new retired Stake Cell must outlive the configured relative CKB block delay before withdrawal. Registry guardian rotation requires a quorum of both the departing and arriving authorities over the same state transition.

A Delegation Cell's Type Script contains a 32-byte commitment to the Registry hash, validator key, beneficiary Lock hash and CKB Type ID. Its Lock uses the same contract with empty args. Registry witnesses carry the commitment preimage, which CKB checks alongside the creation Type ID. The declared principal must equal Capacity in excess of occupied Capacity. This layout occupies 146 CKB. The 40-byte data field shares one sequence slot between creation and a later reduction request. The activation height distinguishes those phases. Each validator's Registry record stores one sparse root for active principal. An update proves the prior leaf keyed by the delegation identity commitment and derives the successor root from the actual funded outputs. Pending principal stays in the unbonding tally. The guardian observer relies on the pinned CKB script for both the aggregate and its root and independently reads the validator Stake Cells. Voting power uses self-bond plus eligible delegated principal. A top-up creates a distinct position so it has its own activation and liability. The first applied Registry snapshot covering creation, including genesis, establishes its earliest liability height.

If total raw power exceeds the configured cap, selection preserves one vote per selected member and proportionally scales the rest with rounded-down integer weights. Validators compute the quorum from the resulting total. The cap bounds power arithmetic while allowing a snapshot to remain usable after a large delegation.

An in-term reduction moves a signed principal delta from counted delegation to a separate unbonding tally. The position keeps its proved activation height. Stake application writes the first matching sequence interval and its CKB block anchor into SystemRoot. The CKB contract uses the same proof format for activation and reduction, checking a live checkpoint at the effective height or later and a canonical CKB snapshot anchor at or after the corresponding L1 transaction. Confirmation sets the cutoff in the same delegation Cell, which starts its relative CKB delay. Settlement can preserve an active successor or release the entire position. The Registry records the lower and upper vote heights of a proved global fault. Both votes in a global or independently proved fault must be at or after activation and before exclusion to charge that position.

Withdrawal of an unreduced position references the matching live retired Stake Cell and its CKB Header. The delegation input carries an absolute block-height `since` at least the retirement output height plus the configured delay. CKB validates the beneficiary payout. Frozen active positions can be settled after authenticated retirement with principal paid as CKB transaction fees and occupied Capacity paid back to the beneficiary. Both withdrawal paths are independently relayable. The Registry retains the validator's entry until active and pending delegation are both zero.

The CometBFT comparison build verifies both signatures over CometBFT 0.38 canonical vote sign bytes. Chain ID, key, height, round, vote type, distinct Block IDs, and the authenticated liability interval determine admissibility. The evidence key has one value for the vote position regardless of vote ordering. A proved equivocation freezes the funded bond and excludes the record from future snapshots. Finalizing the penalty after authenticated retirement forfeits the positive bond as a CKB transaction fee. The residual Stake Cell retains its retirement and evidence state. The fixed Registry limit applies to 256 unsettled Stake Cells. Historical markers occupy one root in the Registry.

The guardian observer checks all live Stake Cells against the confirmed CKB Registry and persists its confirmed L1 anchor and its chosen signing digest for the latest epoch. The native snapshot authenticates an authority rotation with outgoing and incoming quorums over a single digest. `stem stake verify` requires strictly more than two-thirds of pinned guardian weight for the same snapshot and derives power by integer division. Eligible stakes are ordered by decreasing bond then key, limited to the configured committee size, and checked against the configured total and CometBFT bounds. The first epoch starts at height 1. Subsequent epochs start at `1 + epoch * blocks_per_epoch`, with the proposed ABCI update at that height minus two.

This is guardian authentication of a trusted CKB full node's state. Guardians authorize activation and attest confirmed CKB state. The L1 contract enforces activation authorization and the delayed owner spend. The native application accepts an authenticated genesis committee, admits snapshots and emits updates for H+2 activation. The CometBFT verifier authenticates both signed committee handoffs in the L1-backed runner. The Stake Registry contract checks the marker's registry and validator keys, the exact first inactive height, and the native height bound. Competing and premature updates fail against the actual Registry and checkpoint inputs. The permanent Stem marker rejects any subsequent snapshot that attempts to restore the key. After a confirmed withdrawal the L1 retirement root preserves the ban. Stake Cells and the bridge vault use separate L1 scripts and state.

## CKB and xUDT bridge

Each native lane pins a separate L1 vault Type ID. A mint certificate binds the sorted deposit OutPoints, observed vault predecessor and successor, exact Capacity and xUDT increments, and all native transaction outputs. Deposits use permanent SystemRoot keys. The wrapped RISC-V UDT receives mint rights only for the current authenticated Type Group. Native burns reserve a known amount and assign a contiguous lane nonce. The Burn Cell still participates in every applicable Type Group before it is removed from the live set. An L1 release consumes the unique vault and references its pinned direct checkpoint. The CKB vault Type Script checks the checkpoint's code hash, Type ID, Stem network and L1 genesis. It verifies a canonical Patricia path under the authenticated SystemRoot and binds the pending record to the lane, input vault nonce, payout output and successor accounting. The ordinary CKB fee input funds the relay. The vault's custom Lock binds the Type Script, while consolidation, pairwise reserve moves and rotation require a separate input locked by the current CKB multisig. Guardians reconcile the actual confirmed L1 release into Stem after independently checking the native transaction and receipt proofs and the signed successor.

`stem::l1_proof::verify_output` independently verifies a historical CKB output's OutPoint, data and witness commitment with CKB's complete binary Merkle tree. The caller supplies the trusted CKB block hash and confirmation policy. The result carries only the output at creation. Later consumption and current liveness require separate state authentication.

Stem fees extinguish Capacity. On bridged networks the same atomic write updates the `bridge/backing` SystemRoot record. A certified mint adds recognized reserve and circulating Capacity. A burn moves Capacity from circulation to pending withdrawals. A certified payout subtracts that amount from both pending withdrawals and recognized reserve. Moving the reserve between lanes or rotating a vault preserves the aggregate. Unallocated Capacity equals cumulative extinguished fees less total queued fund claims and accrued stake earnings. The accounting excludes L1 vault base Capacity and external L1 fee inputs. Wrapped units remain in their respective lanes.

`/stem/bridge-backing` returns the Strand record and its Patricia path. `/stem/bridge-lane` authenticates one lane's reserved balances, vault OutPoint and both withdrawal nonce frontiers. `/stem/bridge-withdrawal` supplies the pending record and path for a specified lane and nonce. The Rust node and CKB contract share the canonical key, path and hash logic. Confirmed CKB observations depend on the configured guardian quorum and the L1 confirmation policy. Stake Cells are separate L1 resources. The cumulative fee tally at `/stem/extinguished-fees` also covers development fees.

On a bridged chain, `fees/vault-revenue` attributes each block's extinguished fees across the configured lanes by their prior authenticated liquid CKB. An empty parent reserve uses the block's closing reserves. The sorted lane IDs define the order of the cumulative integer quotient, so the resulting differences sum to the exact fee. The Store checks the weights against aggregate liquid backing and commits one revenue record alongside the global fee tally. Attribution reads one reserve record per configured lane on blocks with positive fees. The RPC exposes paths for both records. Fund disbursements remain authenticated by the fund's lane and payout record.

Bridge-enabled genesis may commit four fixed fee shares and CKB recipients. Per-account Strand records advance cumulative queued and paid balances. Each certificate-free Fee Claim checks an expected cumulative queued balance and consumes at most one claim allowance for its account per block. Claims reserve aggregate surplus and liquid Capacity in the native CKB lane, then issue an ordinary pending withdrawal at the next nonce. A separate pending fund marker maps reconciliation to the relevant account and is deleted in the same batch as the settled withdrawal. `/stem/fee-funds` authenticates the fixed policy, the fee tally and cumulative payouts. Fund entitlements use prior committed cumulative fees and overflow-safe integer arithmetic.

The final execution block before an actual committee handoff saves a closed reward epoch under `stake/rewards/epoch/` keyed by little-endian u64. It deducts the epoch's opening allocation from the closing allocation for the validators and delegators fund. Allocation uses effective committee power and the corresponding historical principal and commission. A 144-byte record commits the budget, height and fee intervals, Registry identity and commitment, and committee commitment. It shares the block's atomic write and is queryable under SystemRoot.

On stake-backed networks, a permissionless reward claim opens the historical records. Canonical bundles group by epoch and cover up to 32 positions with one beneficiary. Each position verifies the beneficiary's complete CKB Lock hash. A permanent claim record keyed by epoch, position role and ID carries the accrual transaction and allocated amount. A positive balance under the Lock hash and its aggregate in `bridge/backing` authenticate accrued rewards. A payout consumes the full balance, optionally after crediting new earnings in the same operation. It moves the amount from accrued rewards to pending withdrawals and allocates one lane nonce. Reconciliation removes the pending withdrawal and increases the fund's paid balance while preserving the position records. The same atomic batch commits all accounting and replay records for each action. The CKB vault verifies the existing checkpoint and pending record formats. The production bridge reorganization policy remains open.

## Native weighted Jolteon

The `stem run` replica initializes from genesis. Its Ed25519 peer directory stays in public-key order across epochs. The certified committee determines who can vote. Other configured peers receive and execute blocks as observers. Votes, timeouts and proposals have separate signing domains bound to the network, genesis, epoch and committee. Successor signature domains also bind the authenticated epoch anchor. A compact Ed25519 QC contains an exact-width signer bitmap and signatures in member order. A TC carries one highest QC and the signed high point reported by each timeout signer. Both require strictly more than two-thirds of total stake weight. Leaders use a deterministic weighted schedule. Pacemaker retries do local bounded backoff, and peers retransmit progress certificates and outstanding votes.

A proposal commits a native execution Header and a certified parent. The voter fetches missing payloads and runs all transactions along the candidate branch against a read-only overlay rooted in finalized state. The overlay uses the same transaction verification, scheduled work, resource accounting and state roots as the ABCI path. The replica synchronously records the payload and vote watermark before sending a vote. A child and its certified parent at adjacent views finalize the parent and its ancestry. The store applies each finalized block with its proof and consensus watermark in the same RocksDB batch. A replica more than 192 candidates behind stops signing and requests finality at its first missing height. It authenticates the response under its installed committee, fetches the certified payloads and executes each bounded branch from its exact local tip. Same-epoch recovery preserves the vote and timeout watermarks. The outgoing terminal proof provides the ordinary handoff when replay crosses an epoch. Recovery requires a peer retaining the certificates and payloads. The network tests use a private loopback network and preconfigured peers.

Consensus traffic and transaction relay have separate bounded outbound
queues and TCP streams per peer. On receive, consensus traffic uses a
separate queue from transaction relay and client requests. The replica
handles up to eight waiting consensus events before servicing a waiting
application event. Both share the replica's execution loop. The
transport still uses the canonical Frame and the replica verifies the
same signatures and state transitions.

A client connection can carry sequential `SUBMIT` requests using the
same Frame and admission reply as a fresh connection. The replica
replies before reading the next request and retires the connection
after 256 submissions or one second without a complete next frame.
Submission connections use the existing connection limit and the
application queue. Query and peer paths use their existing streams.
Every frame uses a four-byte big-endian length. Replica writes send
the prefix and body together with vectored I/O. The Rust client
encodes each request directly into a prefixed packet.

Admission checks individual transactions against the finalized tip
on the replica thread. Parallel block execution uses the VM pool.
Admission stages Registry state only when verifying a Stake Snapshot.
Block preparation stages it once to process both snapshot operations
and height-driven committee transitions.
Prepared blocks reuse the parent's decoded stake inputs while the
accepted snapshot, committee, applied Registry sequence and guardian
authority records remain the same. Writes to those keys give the
resulting view a fresh cache. Each staging pass owns its budget and
pending changes. Snapshot installation resets the cache.
The receiver computes the full witnessed wire digest once and reuses
it for deduplication and the client reply. An admitted client
transaction goes to the current and next Leader. The receiving replica
checks the transaction before storing it. The origin retries an
unconfirmed transaction after two seconds, starting with the latest
known Leader routes and then stepping through the active committee.
Only locally submitted transactions are eligible. Each tick processes
at most eight ready retries, and each pending transaction gets at most
one pass through the committee in the same epoch. Finality and expiry
remove the relay entry with the transaction. The next epoch's first
retry uses the new roster. Proposals carry full transaction bytes for
execution on every voting replica. Node status records client and
relayed requests, Store checks, check time and relay retries. Submission
timing separates the wait from a decoded request to the replica's
handler from the time spent building its admission or rejection reply.
Both are local counters and use fixed latency buckets.

The application includes the next epoch, activation height and ordered roster in the authenticated `stake/announced` record at height `A−2`. The final execution height for the outgoing validators is `A−1`. Before starting the successor, the old epoch certifies an adjacent parent and seal pair. The seal commits the same boundary Header in a separate domain. After a timeout, the seal can extend an earlier seal for that boundary, preserving the adjacent-view finality rule. Only executed blocks advance the Header history. The boundary execution, handoff proof and new vote journal commit in one RocksDB batch. Signed handoffs and certified payloads remain available to a replica that rejoins across a committee change.

A pair of conflicting signed votes at one view is portable evidence. Collectors retain pairs they observe. A CKB slashing witness authenticates the historical committee from its epoch anchor and opens the closing height under the live checkpoint's SystemRoot. Epoch 0 uses the genesis roster commitment. Later epochs use a Patricia roster path and the MMR inclusion of their boundary Header. The contract verifies both votes, the validator key, signer index, epoch, view, distinct points and the applicable execution heights. The authenticated epoch closing height and the Stake Cell's liability interval both constrain admissible votes. An open epoch uses an absence proof. The native network uses independent Ed25519 signatures and bitmap QCs. The BLS experiment keeps individual signatures for evidence.

The native CKB checkpoint pins the Type ID, Stem and CKB identities, genesis Header ID and initial roster commitment. Each proposed update opens the currently authenticated roster, verifies finality or an outgoing handoff, and checks an MMR extension from the live input's previous Header. The Type ID enforces a single successor. Relayers pay CKB fees and submit proofs. The handoff updates the roster commitment and epoch anchor atomically with the boundary Header. CKB validates these certificates and history links. Stem validators establish the validity of the execution they certify, and L1 guardians still attest the observed Stake Registry.

Genesis opens the complete CKB Type Script for each vault and for the signed Registry ID. It matches the network, CKB genesis and checkpoint code hash and Type ID. The Registry additionally binds chain ID, minimum bond and epoch length. Vault validation and Stem genesis use the same argument decoder. Each lane ID commits its complete vault Script hash. The node rejects a second checkpoint lineage before opening its database. Long-range trust also depends on the configured L1 code hashes and a recent canonical CKB anchor. [Cross-epoch experiments](BENCHMARKS.md#ckb-funded-jolteon-handoffs)

The native CKB vault and Stake Registry build selects the Jolteon checkpoint decoder at compile time. Both consumers read the pinned live Type ID and authenticated SystemRoot. The Checkpoint code hash, Type ID, CKB genesis and Stem network bind the dependency. The vault authenticates the pending withdrawal while the Registry authenticates retirement and vote liability. Withdrawal reconciliation guardians authenticate the exact burn Header or its MMR inclusion under a later finalized Header. The CKB devnet tests cover CKB and xUDT payouts, retirement, delayed unbonding and native penalties. [Actual L1 transactions and measurements](BENCHMARKS.md#jolteon-withdrawals-retirement-and-slashing)

Jolteon execution timestamps advance strictly in Unix milliseconds. A validator votes for a new execution only if its signed timestamp is at most 1 second ahead or 30 seconds behind the validator's realtime clock. The shared policy is part of the signature domains. Finality authenticates the Header and requires its child to advance time. Validators need synchronized clocks for liveness and depend on honest quorum weight for approximate wall-clock accuracy. The pacemaker uses a monotonic local clock. Certified historical execution is replayed with the original Header time, and the epoch seal repeats its boundary Header without advancing the ledger. Contracts and time locks use the parent execution context. [Jolteon paper](https://arxiv.org/abs/2106.10362) [Aptos proposer-based blockchain time](https://github.com/aptos-labs/whitepaper/blob/main/english/main.tex)

An observer can restore directly from a canonical state image and an independently pinned execution Header ID. The importer verifies the preceding committee handoffs and exact finality certificate against genesis, checks the pin, then reconstructs and verifies both Patricia roots and the history frontier. It installs the state and a permanently non-signing journal in the same RocksDB batch. The observer resumes from the checkpoint using the normal execution path. Signing replicas keep their original vote journal and use retained certified replay. Ordinary replicas prune old execution records, Header indices, proposal bodies and height-indexed finality records in the same atomic batch as each finalized block. An ordered epoch and view prefix bounds the database deletion to the expired proposal positions. Verified handoffs, the active epoch anchor and signed equivocation evidence survive pruning. Archive replicas retain the complete execution history. The CometBFT comparison uses the frozen baseline or the separate `comet-benchmark` feature.

The sparse checkpoint publisher is a separate CKB wallet process. It verifies the archive's finalized Header against the certified Header, schedules regular publication by finalized time and treats outgoing handoffs as mandatory. CKB witness verification uses the same bundle builder as the manual proof CLI. A synchronous journal records the exact signed transaction before submission and the next roster only after canonical inclusion reaches the configured depth. The previous confirmed block hash pins the local publication history. The publisher pays from its own Sighash Cell and carries the predecessor's Capacity into each update. [Publisher operation](OPERATIONS.md#run-a-sparse-ckb-checkpoint-publisher)

A publisher can adopt a confirmed sequence from the pinned seed or its
journaled OutPoint. Adoption resolves the actual CKB input, output and
group witness positions, verifies the pinned checkpoint Script and each
native finality and MMR extension, and rechecks the canonical CKB block
and confirmation depth before persisting the final tip. A conflicting
pending update is cleared after its predecessor is spent on the adopted
chain. The fee wallet follows its own locally confirmed change through
adoption and funded updates.

An optional CKB Indexer lookup discovers confirmed successor hashes by
the exact checkpoint Type Script. After the journaled OutPoint is spent,
it reads candidates in ascending CKB block and transaction position.
Each batch passes the same canonical adoption checks and writes one
journal update. An immature spend leaves the journal at its current
anchor, and consecutive updates in the same CKB block remain discoverable
after a partial catch-up.

The PoW Security Fund can pay a dedicated permissionless CKB Lock. Stem pins that Lock to the same checkpoint Type ID and networks used by the native vault. On CKB the Lock constrains the exact two-input, two-output checkpoint update and preserves both checkpoint capacity and the fund's change Lock. Only the transaction fee may reduce fund principal. The checkpoint contract independently verifies the certificate, including a mandatory committee handoff eligible before the regular interval. Merging two or more pots preserves principal and requires separately funded fees. The publisher journals a selected fund transaction before sending and tracks the wallet change independently across sponsored updates. [Fund operation](OPERATIONS.md#fund-ckb-checkpoint-fees)

## CKB Cell references

An ordinary Strand transaction can carry one certificate over up to four reference records ordered by OutPoint. All records share a CKB genesis and observation anchor. The complete Lock digest and transaction ID cover the certificate. The validator checks the signature quorum and exact CKB anchor from the parent overlay before exposing a reference to any RISC-V group. Reference records are separate from resolved Stem Cells and executable dependencies.

Each bridge configuration keeps a bounded 64-entry cache of successful quorum checks keyed by the exact certificate bytes, including signer indices and signatures. Its network, CKB genesis and guardian generation, keys and weights are fixed. Each use checks the exact committed anchor and live-reference age, and the authorization binds the complete witnessed transaction. Intercell and StemVM share the authenticated immutable records. The receipt charges the same Ed25519 cycles and logical reads on every use. See the [verification measurements](BENCHMARKS.md#reusing-verified-ckb-reference-sets).

The bridge anchor record commits CKB height and hash plus the Stem adoption height. The host bounds live-reference age in accepted Stem heights. Guardians sign historical records only after verifying CKB creation inclusion. For live-at-anchor records they also read the live Cell at a stable descendant CKB tip and verify it matches the historical output. Reference signing has a separate journal and signing domain. L1 facts depend on the configured guardian quorum and confirmation depth.

A fee-exempt `L1AnchorUpdate` lets the bridge guardian quorum advance the anchor during periods without deposits or settlements. It uses a distinct signing domain and shares the bridge guardian journal for reorg detection. Each update extends the exact accepted predecessor and charges its separate operation, byte and signature budget. Only the anchor record changes. Vault OutPoints, reserve accounting and nonce state remain stable. A transaction can read the new anchor once it is in the parent state.

The optional native storage metrics query classifies bytes in each actual
RocksDB WriteBatch before its shared synchronous commit. It also exposes
RocksDB WAL, flush and compaction counters for the database as a whole.
Measurements are local to a process. A proposal body already durably stored
under the same key is reused when its encoded bytes match. The replica
continues to synchronize vote state, execution, finality and retirement
ranges at the existing atomic boundaries. The separate history index can
be measured in its own process.
[Storage experiment and RPC](OPERATIONS.md#measure-native-storage-and-sustained-payments)

## Browser verification

The browser SDK links the same Strand transaction codec, Patricia and receipt
proofs, and storage-independent Jolteon verifier used by the native CLI.
A client pins the network, genesis execution ID and initial committee, then
checks handoffs in proof order. A verified Header authenticates transaction
and state proof inputs. A signing context verifies every supplied Live Cell
against one finalized root and accepts Header Deps only from the same trust
anchor. The SDK computes the native authorization digest, and the caller
supplies the group's seal. [Build and verify](sdk/wasm/README.md)

## Application security domains

A CKB Type ID cell holds independently funded service positions. Its policy fixes the CKB and Stem identities, the eligible roster, the original bond denominator and the application challenge interval. Active domains deliver paid observations of a specified CKB Live Cell. The CKB transaction opens that precise Cell Dep, recomputes its commitment, checks a weighted Ed25519 certificate and divides the buyer's escrow among signers and one typed receipt. Domain and request consumption bind the receipt to a single initial escrow. The holder can later consume the receipt while its historical inclusion remains provable.

Two conflicting signed reports for the same request transfer the responsible bond to the pinned penalty address and excludes that position. Retirement freezes service and starts a relative CKB challenge period. Remaining capital is released only after the latest retired state matures. Stem references the application's typed CKB output through its existing CKB observation proof. That proof uses the bridge's separate guardian quorum and does not change either the Stem committee or the application's service quorum. [Developer workflow](OPERATIONS.md#exercise-a-paid-ckb-security-domain)

The Rust security-domain audit builds a report through a selected CKB Cell or closure. It checks the Type ID creation and follows each consumed OutPoint. CKB transaction and witness inclusion bind service certificates and actual payments, including payments to members later slashed. The report separates service income, locked capital, penalties and returns. Its caller authenticates the CKB block hashes. An RPC-based collector can also query whether the selected Cell remains live. [Audit workflow](OPERATIONS.md#verify-security-domain-capital-and-income)

## Intercell Messaging

Intercell uses one shared codec for the message identity and payload. Its two reserved Group Actions enter the ordinary transaction signing digest. A transaction can carry up to four Intercell actions, one per consuming Lock group. Execution checks the endpoints and nonces of every action, and records each full message under its direction and ID in SystemRoot. The per-transaction record set and the block overlay prevent duplicate use. The commit applies the entire set with the application changes. The receipt concatenates the message IDs in Lock Group order. Ordinary byte, state access and VM accounting applies to each action.

A CKB Mailbox is a Type ID state Cell with a pinned owner Lock and checkpoint identity. Sending advances its local nonce and records the outgoing message on the successor. The inbound sparse root commits to each delivered message ID. Each CKB delivery opens an outgoing Stem message at the pinned Jolteon checkpoint or at a prior execution Header proven by that checkpoint's MMR. The checkpoint contract enforces committee handoffs and preserves history continuity across epochs. The mailbox checks the frontier root and exact leaf count, then uses the authenticated Header's SystemRoot for the Patricia proof. The delivery also proves non-inclusion in that mailbox's current replay root. The owner Lock authorizes spending the mailbox in the same transaction as any receiver application effects.

The reverse path uses an authenticated CKB historical output. Stem pins the Mailbox binary and requires the mailbox to use the genesis-bound checkpoint that also serves the bridge and configured Stake Registry. Execution finds the unique historical reference matching the full message and Mailbox identity among the transaction's certified CKB outputs. Additional references remain available to the application and incur the regular verification charges. The recipient Lock authorizes the ordinary Stem transaction. Confirmation and ancestry depend on the guardian quorum and its journal. Incoming and outgoing records do not issue or reserve assets. The generic wire type is independent of the CKB proof and transaction formats.

A relay can rebuild its inbox index from a live Mailbox OutPoint and the pinned complete Type Script hash. The recovery command uses a CKB full node's committed transactions and finds the actual predecessor by Type Script. For each delivery it verifies the transaction and witness Merkle paths against the selected canonical block before reading the witness. It checks each send or replay-root transition and rechecks head liveness. The scan has an explicit transaction-read budget and runs outside consensus. The rebuilt IDs are ordered from creation to head. Relays with a maintained index can give the witness builder a single replay path. The builder checks that path against the supplied live root and derives the same successor with the same verifier as the Mailbox contract.

### Native UDT settlement over Intercell

The Stem application UDT uses a complete issuer Lock hash and series identifier. Its Type Script enforces conservation outside issuer-authorized supply increases. The Stem escrow validates the precise token and Capacity deltas, the authorized Intercell action and the endpoint pair. The asset route derives the complete CKB Mailbox Script from its Type ID, the asset and escrow hashes, fixed code hashes and the shared checkpoint. A CKB wrapper identifies the route by that Mailbox Script hash. Its supply changes require consumption of the same Mailbox. The Mailbox's dedicated Lock constrains actual mint outputs or actual burned inputs, and the Mailbox Type Script authenticates the message and replay state. The CKB Lock and Type Script enforce both obligations in the same transaction.

The app sends no money through the generic message handler. Native UDT stays in the escrow and moves under its Type Group. The CKB wrapped UDT can be transferred independently. A CKB burn updates the mailbox atomically, and Stem consumes the certified return once when moving the underlying UDT. Both escrow bonds stay fixed. The settlement path currently accepts only single-Type instances of the asset.

### Spore V2 settlement over Intercell

The CKB endpoint pins the exact deployed Spore V2 code and one complete original Type Script hash. It allows an import message only with an actual CoBuild-authorized transfer of that Cell into custody. A release spends the same original Type at the same capacity and with byte-identical data, under a proof that Stem burned the representation. The original CKB Type Script validates both transfers. The CKB inbox root protects the original against consuming an earlier return after a later deposit.

Stem derives the canonical representation Type Script from the fixed endpoint Lock. Its Type Group permits ordinary ownership transfers with immutable Spore bytes. Creation and removal require the consuming endpoint Lock, which checks one actual mint or burn under the appropriate Intercell message. The fixed bond Cell enables later cycles without reassigning the mapping identity. The current route supports the basic immutable Molecule profile without a Cluster or MIME extensions.

### Native compound object settlement

A Stem-issued object has separate identity and transfer-approval Type Groups. The identity script derives the object's ID from the first spent OutPoint and the object's creation output index under the network domain. It commits both chains' approval Locks and checks that the immutable object carries the expected policy Type. The native policy executes independently and requires a consumed input under the Stem approval Lock.

The CKB representation pins the full Mailbox Type Script, its authorized Lock, and a CKB approval Lock. CKB's single Type Script enforces the mapped approval policy on issuance, local transfers and burns. The scoped rule deliberately models a separately authorized transfer. The tracked content still passes the ordinary immutable object checks on both chains. The source CKB or Stem application checks its remote recipient against the reserved destination custody Lock before committing a cross-chain movement.

Release restores the original Stem Cell data, both complete Type Scripts and the original capacity from escrow. Stem validators authenticate the CKB burn through the existing observation path. A later CKB reissue has to come from a newly authenticated export. The original CKB Spore V2 path retains its own canonical Type and lifecycle.

### Metatype on CKB

One Data2 Type Script owns a Metatype deployment. Its args pin an
immutable adapter table and CKB genesis. Cell Data either includes
the complete Manifest or commits a shared definition by CKB data
hash and carries its own state strings. The dispatcher, adapters
and custody gate resolve definitions from actual Cell Deps. Each
VM validates and indexes a shared definition once, then borrows
Script and state bytes for the logical view used by validation.
The dispatcher meters expanded bytes and derives a union of
logical Scripts and absolute input and output indices from the
actual transaction, and spawns each pinned RISC-V adapter once for
its complete group. The existing CKB Locks authorize consumption.
The standalone implementation validates UDT conservation and an
independent transfer policy over the same amount state. The first
extra witness can route Messages, Proof Data and Seals by deployment
and virtual Script identity. An immutable object adapter pins a
separate approval group which verifies detached Ed25519 authority.
An immutable custody policy binds a deployment to one Intercell
Mailbox Type ID, an exact original Manifest and a designated data
Script. The generic Stem escrow and CKB gate enforce complete
projection, verified mint and burn, and recipient binding. The
portable object and approval groups still execute at each boundary.
Revisable custody uses the same escrow, gate and adapters. The
native and portable identity Scripts share the monotonic revision
rule. Export seals the current state, and return restores the exact
state authenticated by the CKB burn. Both chains require the
separate authority and ownership checks. The CKB import reserves
mailbox Capacity for the largest permitted return. Stem accepts an
authorized storage top-up without releasing any of the original
principal.
The object identity Script checks membership of its required approval
Script and can compose with other constraints. The dedicated object
escrow enforces its two-Script limit. Metatype custody pins the
complete Manifest and runs the extra adapters. Its independent state
limit Script and adapter resolve the same content-addressed rule
record from actual dependencies on both chains. Republishing that
record changes its OutPoint and preserves its rule identity.
The state limit also supports a CKB Type ID registry with a stable
Type hash and an adjustable ceiling. Stem resolves a live reference
at its committed CKB anchor, and the portable adapter resolves the
actual CKB dependency. Decreases apply to new or edited state.
Byte-identical transfers preserve previously admitted state. An
oversized import or changed return requires the pinned dispatcher,
gate, mailbox, adapter and native escrow to authenticate admission
on the originating chain.
An independent approval group reads a rotating Ed25519 key from a
CKB Type ID registry. Both VM hosts verify an exact-transaction
seal with the registry generation. CKB reads the actual dependency
and Stem reads a live record at its committed CKB anchor. Rotation
and freeze updates preserve the object and approval Script
identities during custody. Both hosts require an active record.
A frozen return leaves its mailbox message unconsumed and its
escrow live.
An independent append-only group binds the complete object identity
and checks one old and one new state on either host. It preserves the
MIME type and content prefix when the revision increases. Prefix
extension composes across multiple CKB transfers, so the original
Stem group can verify the aggregate change on restoration.
The bounded workflow group covers a different case. Each review
or completion commits the preceding head and needs its own key.
CKB validates each step before changing the object. Stem replays
the ordered signatures from the escrowed head using its existing
Proof Data and verifies the resulting authenticated return state.
The recovery CLI uses an authenticated CKB anchor and the original
Stem escrow Cell supplied by the caller. It verifies CKB transaction
and witness proofs against parent Header hashes, follows actual
OutPoints, and prepares the proof for that same native Type Group.
This runs off chain using read-only CKB RPC.
A royalty Type Script pins an object and both chains' payee Locks.
Stem and the CKB adapter verify a fresh untyped payment output on
Lock changes. The output marker binds the full royalty Script and
the deployment scope. Stem applies the rule to export and release,
and CKB applies it to transfers of the mapped object.
The fungible custody profile reuses the same gate and escrow.
The source asset Type requires an independent amount-cap Type
on every Cell. CKB checks the exact issued or burned total and
runs both adapters. Partial redemption conserves the principal
still held on Stem and represented on CKB.
The multiasset profile also carries several UDTs in one Cell. The
native amount map uses full asset Script hashes as keys, and the CKB
Manifest stores each quantity under that same Script. The escrow and
mailbox sum and compare each asset independently while the original
UDT and cap groups validate the local transaction. A return can
select any nonempty subset. A zero balance or delta keeps its
asset key and the full constraint set. A later message with the
next mailbox nonce can return the remaining principal.
See the [Metatype specification and threat model](METATYPE.md).

## Historical proofs and CKB checkpoints

Archive replicas preserve committed Headers. The proof index follows those records in a separate RocksDB database and checks the predecessor ID and MMR root before advancing. It persists only derived history nodes and progress. Each index update is independent of the application's commit. An archive reader can generate inclusion and append-only proofs at earlier retained heights.

The CKB Checkpoint Type ID contract verifies the incumbent authority's quorum, strict sequence and height, the actual predecessor OutPoint and an exact MMR extension. The extension starts by appending the old published Header itself to its prior peaks. Authority rotation requires both independent quorums. The signed record also binds the complete Stem Block ID and signed successor Block ID, the successor's current and next committee hashes, the authority-verified Stake Snapshot and the verified handoff bundle. AppHash is derived from the network, height and recorded Stem Header ID.

The off-chain preparer authenticates the Stem genesis and the CometBFT successor, verifies its handoffs, and checks the Patricia Stake Snapshot proof independently. On CKB, the checkpoint authority certifies the successor and handoff interpretation. Clients still need a trusted view of the CKB chain, and can independently recheck the available Stem bundle.


### Direct CKB finality verifier

The direct Checkpoint Type ID stores the last authenticated Comet Block
ID, the next committee hash and the canonical native Header. The deployment
pins chain ID, network, L1 genesis, genesis committee and genesis AppHash.
The public witness contains up to eight consecutive full Comet headers,
the PartSet IDs and rounds, weighted Ed25519 precommits and native Headers.
A repeated committee within one proof is referenced by an empty committee
entry. The verifier recalculates the Comet v0.38 header and validator-set
hashes and the Stem H+1 AppHash. Parent Block IDs and native Header IDs
enforce continuity at both levels. The same CKB lock pattern and Type ID
uniqueness protect the update Cell. The Go exporter accepts public Comet
RPC responses and the host CLI uses the same Rust verifier as the contract.

Exact progression requires relaying every intervening commit. The contract
does not enforce a trusting period or validate the underlying Stem execution.
Bridge vaults use this Cell for payout proofs. The Stake Registry uses it for effective retirement proofs. The authority-published
checkpoint serves its own archival and verification path.

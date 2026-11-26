# Metatype

Metatype represents a set of independent Type constraints in a single
CKB Type Script. A Metatype Cell has a normal CKB Lock. Its Type
Script identifies a deployment that derives and dispatches all
applicable virtual Type Groups. Stem retains native Polytype Cells
and their existing Script Groups.

The standalone CKB dispatcher, adapter table and virtual group ABI
run independently deployed UDT, transfer policy, object identity
and object approval adapters. Standalone object cases run against
both CKB Metatype and four native Stem Polytype replicas. The
custody profiles cover immutable objects, revisable objects with
composable state constraints, and fungible UDTs with independent
per-Cell amount limits. Each profile
can store repeated Script sets in a shared dependency. [Implementation status](STEM-EVOLUTION.md)

## Historical sources

Phroi identified the problem of an existing protocol occupying the
Type slot and investigated additional validation, independent data
regions, and using witness commitments to reduce occupied Capacity.
Jan distinguished Lock/Type callbacks from a Type/Type dispatcher.
The iCKB discussion illustrates the risk of treating an output
containing a familiar Lock as evidence that its creation rules
executed. [Nervos Talk discussion](https://talk.nervos.org/t/nip-allow-some-output-lock-scripts-to-validate/8636)
· [iCKB issue #19](https://github.com/ickb/whitepaper/issues/19)

Xuejie's xUDT design introduced an ordered ScriptVec, optional witness
preimages, and local data for each extension. Its extension is scoped
to the surrounding xUDT group. Metatype derives each virtual group's
membership across all manifests in one deployment. The CellScript
discussion also identifies receipt Cells and read-only Cell Deps as
composability mechanisms where an existing protocol owns the Type
slot. [Extensible UDT](https://talk.nervos.org/t/rfc-extensible-udt/5337)
· [CellScript discussion](https://talk.nervos.org/t/cellscript-a-dsl-for-cell-based-contracts/10193/13)

Stem already supplies native Type Groups, the portable data needed
to identify their Scripts, and finality-backed Intercell Outbox
proofs. The current compound object path is specialized to a fixed
object and transfer policy. Its CKB script checks at most one object
and one approval authority. Metatype adds a shared projection and
dispatch mechanism. [Native group construction](ledger/src/lib.rs)
· [Existing compound-object contract](contracts/src/bin/ckb-stem-object.rs)

## Deployment and identity

The physical Type Script is a Data2 reference to the dispatcher.
Its args commit the actual CKB genesis hash, a nonzero deployment
namespace and the data hash of a canonical, immutable adapter table.
Every spend and creation supplies that canonical genesis Header as
the first Header Dep. The dispatcher checks height zero and its hash.
A different chain cannot instantiate the pinned deployment with a
foreign genesis. All Metatype Cells in a
deployment use the same full physical Script. Their manifests and
state vary in Cell Data. The CKB kernel therefore invokes the
dispatcher once for all of those Cells. Independently configured
deployments have independent assets and validation domains. A
bridged lane pins the full destination Script and source network
identities.

An adapter table maps a logical Script's code hash and addressing
mode to an immutable CKB adapter ELF data hash. The table is shared
as a Cell Dep and is committed by the deployment. Script arguments
remain per constraint. An adapter undergoes review and differential
testing before a deployment is configured to use it. The dispatcher
fails on an unknown logical code identity, ambiguous or unavailable
table data, missing adapter ELF, or a nonzero adapter result.
A portable adapter must perform bounded validation and must not
spawn other processes, execute other binaries, or dispatch further
adapters. The deployment's code allowlist pins that reviewed code.

The logical Script identity hashes its complete canonical CKB Script
encoding, including its addressing mode and arguments. Virtual
groups use the deployment scope and full logical identity. Bridge
accounting additionally uses the original asset or object identity
and source genesis. A manifest hash commits the full ordered set of
logical Scripts. Manifest identity, logical Script identity and the
destination physical Type hash are distinct.

## Manifest and logical view

The Metatype Manifest is a strictly ordered set of complete
canonical Scripts. Each entry has one bounded state byte string.
Canonical order is lexical order of the encoded Script bytes.
Duplicates, invalid Script addressing, trailing bytes, oversized
scripts, and oversized state are rejected. Position is an encoding
detail and has no authority over membership.

The following fields are concatenated. Integers are little-endian.
Molecule Script encoding is exact, with no trailing fields.

| Record | Reference encoding |
| --- | --- |
| Type args | 32-byte CKB genesis hash, 32-byte deployment namespace, 32-byte adapter table data hash |
| Inline Cell Data | one-byte entry count from 1 to 16, then each logical Molecule Script and state byte string, each prefixed by its own u16 length |
| Shared Cell Data | zero byte, 32-byte CKB data hash of the definition, then one u16-length-prefixed state string per definition entry |
| Shared definition Cell Data | inline encoding of the complete Script set with every state string empty |
| Adapter table | one-byte entry count, records of logical code hash, one-byte addressing mode and adapter ELF data hash, then custody tag and optional custody policy |
| Custody policy | mailbox code data hash, mailbox Type ID, gate code data hash, full source Manifest ID and data commitment |
| Gate args | custody tag, source escrow Lock hash, physical Metatype Type hash, mailbox code data hash, mailbox Type ID, Manifest ID and data commitment |
| Native escrow args | CKB genesis hash, physical Metatype Type hash, checkpoint code data hash, checkpoint Type ID, custody tag and custody policy |
| Adapter argument | full physical Type hash, one-byte boundary and custody scope, u16 length and complete logical Script, input and output index lists, then optional group witness fields |
| Index list | one-byte count and strictly ascending absolute u16 transaction indices |

Adapter table records are ordered by logical code hash and addressing
mode. The one-byte custody tag is zero for a standalone deployment,
one for immutable custody, two for fungible custody, three for
revisable custody and four for multiasset custody. The custody
tags carry the same five fields in the adapter table. The data
commitment identifies the original data-bearing Script for the
first three custody profiles and the ordered asset keys for the
fourth.
The dispatcher derives the boundary value from the authenticated
deployment and actual input and output state. The low two bits of
the scope byte encode 0 for standalone, 1 for custody transfer,
2 for CKB import and 3 for return to Stem. The high six bits carry
the custody tag. A standalone invocation uses scope zero. The
dispatcher passes the argument as one hex C string. The
adapter verifies that the spawned VM sees the same physical Script
hash. The logical addressing modes are Stem Data (0) and Type (1).
The physical dispatcher and resolved adapter binaries use CKB Data2.

A Metatype Cell contains 1–16 constraints and at most 8,192 bytes in
the complete inline view. Script and per-entry state sizes are
limited to 256 and 2,048 bytes. A transaction permits at most 32
virtual groups, 128 actual Cells on each side, 64 matching Cells on
each side, 128 KiB of expanded matching input plus output Cell Data,
and 128 resolved Cell Deps. The adapter table contains 1–64 entries.

The dispatcher and adapters authenticate a shared definition by the
data hash of an actual resolved CKB Cell Dep. The definition has at
most 4,161 bytes and must use the canonical inline shape. Each
referencing Cell supplies exactly one state string per definition
entry, in Script order. The state strings always live in the
referencing Cell. Validation rejects a missing definition, a
definition that itself references another dependency, nonempty
definition state and invalid state cardinality. A definition can be
republished at a new OutPoint with identical bytes. A reference
requires a live copy of its preimage at spend time. Applications
retain and publish those bytes as needed, along with the adapter
table and program dependencies.

The two storage forms resolve to the same logical Manifest and can
participate in the same virtual group. The dispatcher counts the
full expanded length for per-Cell and transaction limits before
spawning any adapter. Each executing VM authenticates its actual
dependencies and caches validated Script ranges in the definition.
The logical view borrows Script bytes from the definition and
state bytes from the participating Cell. CKB cycles meter that work.
Witnesses carry current transaction material under a separate size
limit.

The dispatcher reads the actual inputs and outputs from CKB,
matches the full deployment Type hash, and decodes every matching
Cell, on both sides. It traverses the Script union in canonical
order. For each group it supplies the exact ascending absolute
input and output indices, the complete logical Script, the
deployment, and authenticated access to per-entry state. Adapters
can read other CKB transaction inputs, outputs and dependencies at
their actual indices. In particular, an input whose last instance
of a constraint is removed still dispatches that constraint with
its inputs and an empty output list. Every group must pass.

The physical CKB Group Input/Output indices refer to all deployment
Cells. The portable ABI uses explicit virtual indices, so receiving
a child PID does not change CKB's syscall group. A Metatype Adapter
uses its logical Script and hash from the dispatch context. The
CKB Lock still checks ownership over the actual transaction.

## Group witness and detached approval

The first witness after all actual transaction inputs can carry a
Metatype witness envelope. A missing witness or one with a different
prefix represents no Metatype witness. The envelope begins with
`Stem Metatype Witness\0` and a one-byte entry count of 1–32.
Entries are sorted by the 32-byte physical Type hash then the 32-byte
logical Script hash. Each entry carries those two hashes, then a
nonzero u16 action, and Message, Proof Data and Seal byte strings with
their own u16 lengths. Field limits are 512, 1,024 and 128 bytes.
The complete envelope is limited to 32 KiB.

The dispatcher rejects a malformed envelope and an entry for its
own deployment with no actual virtual group. It gives each group
only its own entry. Entries for other deployments are handled by
their dispatchers. A row does not imply any verification obligation
and cannot add or suppress a group. Independent witnesses for other
applications can follow the envelope. The normal CKB Sighash Lock
commits extra witnesses in its existing signing digest.

The object approval adapter uses a detached Ed25519 seal. Its
domain is `Stem Metatype approval\0`. Its digest includes the
physical Type hash, logical Script hash, CKB raw transaction hash
and action, then the Message and Proof Data with independent u32
lengths. It excludes Seal bytes so different authorities can sign
independently. The raw transaction hash commits input OutPoints,
Cell Deps, Header Deps, outputs and output data. The adapter checks
the Message against the actual output Lock hash and the Proof Data
against the actual object content hash before verifying the seal.
A copied seal fails for a new input, recipient, deployment, group
or changed proof.

## Portable immutable object

The identity adapter accepts a single output and either zero or
one virtual input. Local creation derives the ID from the physical
deployment, the first actual input OutPoint and absolute output
index. A transfer preserves the object's data byte for byte. The
identity Script requires its exact companion approval Script in
both input and output manifests. The companion identity derives
from the identity Script hash, independent Ed25519 authority and
the approval code committed by the identity Script.

The approval group checks that its members still carry the original
logical object Script. Action 1 approves creation, and action 2
approves transfer. A valid seal cannot override immutable content,
mandatory membership, or the one-head rule. This local profile does
not permit destruction or import. Custody-based export and return
need separate bridge authorization as specified below.

The native control uses the existing Polytype identity and policy
ELFs. Both fixture paths exercise the same content bytes, object
roles, ownership changes, independent permission and decision
cases. Their authorization carriers differ. The native policy
requires a separate signed authority input and the CKB Metatype
policy verifies a detached seal. Neither fixture establishes
binary or signature portability for arbitrary contracts.

## Execution choices

CKB-VM spawn and wait let the dispatcher pass one bounded context at
a time while loading independently pinned RISC-V code. CKB keeps
the actual transaction context, and the portable ABI defines the
virtual group. An exec-only dispatcher would relinquish control
before checking the other constraints. Dynamic linking can reduce
startup cost but shares guest memory with callees. SSRI can name
and version portable methods, while a method number alone leaves
group projection and data availability unspecified. An interpreter
with a restricted instruction set could make a sandboxed portable
profile enforceable without a reviewed ELF allowlist. It requires a
second execution engine and a separate differential cost study.
A future CellScript backend could target the portable ABI for
constraints whose source only uses available context.

Compatible adapters directly verify rules over the projected Cells,
the actual transaction and authenticated signatures. Rules relying
on Stem history, Lineage, custom syscalls or mutable external state
need separately authenticated inputs and a protocol-specific
adapter. Old binaries that use the active physical Script or native
Group Input/Output have different inputs in a spawned CKB VM.
Their evaluation cannot be inferred from a zero exit code. The
bridge accepts only a portable profile that accounts for every
mandatory source constraint.

## Cross-chain invariant

Each custody profile pins the original complete Script set,
the state projection, the CKB deployment, and the
Intercell checkpoint and mailbox. The same escrow and mailbox gate
ELFs serve each compatible deployment. A route must pin a reviewed
adapter for every source Script.
The caller verifies the configured deployment and its committed
table when choosing the Stem escrow Lock.

The source Manifest ID hashes the ordered sequence of complete
Molecule Script hashes. The native escrow derives it from actual
Type Scripts on **both sides** of the transaction. On CKB, the
same Scripts and arguments are present in the Manifest. In the
single-state profiles the designated Script carries the source
Cell Data and all other entries have empty state. In the multiasset
profile each asset carries its own quantity and the remaining
entries have empty state. The custody profiles use Data-addressed source
Scripts. Both the gate and the dispatcher resolve shared CKB
definitions against the actual dependency hashes.

### Immutable objects

This profile requires exactly one represented Cell for an
ownership transfer, with identical Manifest and state in the
complete logical view. Its independent adapters enforce the
source object identity and transfer approval. The checkpoint
and mailbox commit the logical Manifest ID and original Cell Data.

An escrow consumes and recreates a separate untyped empty bond
Cell and one identified asset. Export checks the original asset
moves from its owner to custody. The signed Intercell Action
commits the destination, the complete Manifest ID, recipient
Lock hash and immutable data. The outbound nonce is derived from
the consumed bond OutPoint. Stem's normal Lock and all the original
Type Groups validate the actual transaction. A portable profile
requires its data-bearing original Script to enforce unique
identity, and users supply the corresponding original application
authorization.

On CKB, import and return require the unique Intercell mailbox
configured in the immutable adapter table and a consumed Lock of
the pinned gate code. The dispatcher checks this before selecting
a boundary context. The gate independently checks the exact
Metatype instance on the correct side of the mailbox transition.
It binds the projected Manifest, immutable data and recipient
to the Intercell record. The standard Intercell Mailbox Type
checks Jolteon finality and Outbox inclusion on import, and rejects
replay with its sparse consumed root. On return it checks the
outgoing message nonce. The gate and dispatcher run for actual
spends and creations, and the mapped constraints run individually
for each virtual group. The object adapter requires its companion
policy on every represented head. The policy adapter requires
the original object's paired CKB authority Lock as a signed input
for import, transfer and return. It mirrors the existing source
authority policy. The separately deployed standalone object
profile uses detached seals.

A return consumes the CKB representation and produces the mailbox
message, preventing that representation from being spent again.
Stem authenticates its historical output through the established
CKB guardian and anchor model, consumes the inbound Intercell
nonce, and releases the original Cell with identical data and
Scripts to the specified native Lock. The original Type Groups
execute on release and can deny it. The bond and original asset
Capacity are preserved. The CKB representation has independently
funded storage Capacity.

### Revisable objects

This profile pins an original identity Script with one extra mode
byte, `0x01`, after the four immutable identity fields. The mode
is part of the full logical Script hash, and the exact approval
Script continues to commit that hash. State is an eight-byte
little-endian revision followed by canonical plain object content.
The entire state is limited to 2,048 bytes. Native creation starts at
revision zero.

A transition with the same revision must preserve all state bytes.
Changing the state requires a strictly higher revision. Revisions
can skip numbers so an eventual return can apply multiple verified
CKB transitions at once. The native identity and the Metatype identity
adapter use the same validation predicate. Both keep one live head and
require the independent approval Script. Native and CKB owners still
satisfy the respective consumption Locks. The approval groups check an
input signed by the configured authority for each chain.

Stem export preserves the exact current state across the owner-to-escrow
transition. The CKB mailbox authenticates a finalized export and its
payload binds the imported state, recipient and Manifest. Each CKB
transfer or revision runs the identity and approval adapters. The
return message commits the exact consumed CKB state. Stem consumes the
authenticated message, compares its state against the escrowed revision,
and executes both original Type Groups over the restoration. The
restored Cell retains both original Scripts and at least the original
Capacity. If the authenticated state needs more storage, a normal
authorized Stem input can fund the difference. The bond and asset
principal in escrow cannot be reduced. On CKB, the import must fund
the mailbox for its largest permitted return record. The funding is
Capacity in the mailbox Cell, and future transitions retain it. This
keeps a larger revision redeemable under the same mailbox. The
two-chain checkpoint, CKB guardians and one-time mailbox rules are the
same as in the immutable profile.

The identity adapter requires its exact approval Script. The custody
Manifest can include additional portable constraints. The dedicated
two-Script bridge still requires exactly its identity and approval
Scripts at both boundaries because it has no adapter table.

### Object state limit from an external record

An object can carry an independent limit on the length of its complete
encoded state. This includes the revision prefix and object encodings.
The third Script's arguments are the complete identity Script hash,
a one-byte rule source, and its 32-byte commitment. Source `0` uses
the CKB data hash of an immutable record. Source `1` uses the full
CKB Type Script hash of a rule registry, followed by a 32-byte
custody code commitment. The ceiling record is exactly one
little-endian u16 from 1 through 2,048. The identity group checks
canonical state, the approval group checks authorization, and the
consumption Locks enforce ownership.

For source `0`, both hosts resolve the rule from an actual Cell Dep and compare its
data hash. The configured bytes can be published independently on
each chain and at a new OutPoint. The rule's integrity comes from its
content hash. Availability requires a live copy of those bytes on the
chain where the transaction executes. In this profile the CKB adapter
checks both sides of transfers and the represented side of each
authenticated boundary. The native group requires the same object
and constraint in its successor, including when the object enters or
leaves Stem custody. Source Cell Data remains in the identity entry
of the CKB Manifest. The limit entry has empty state.

The escrow and adapter table pin the three complete Scripts. Changing
the immutable rule bytes changes their Manifest commitment. Republish
the same bytes to recover availability.

### Live registry with adjustable ceilings

With rule source `1`, the application configures the audited CKB
`metatype-rule-registry` Type code and a specific Type ID. Creation
binds the first consumed OutPoint and output index using CKB Type ID
rules. A transition must recreate exactly one registry Cell with the
same Type and Lock and a different ceiling in range. The consumed
Lock authorizes increases and decreases. The registry keeps its Type ID
and stays live.

CKB resolves exactly one actual Cell Dep with that full Type hash.
Stem resolves exactly one typed `LiveAtAnchor` CKB reference whose
Type hash matches. The existing CKB guardian quorum must authenticate
it at the current committed anchor. StemVM exposes the typed projection
through syscall 2130. A historical inclusion supplies no live rule
data. The caller supplies the attestation for every transition that
executes this group. A return includes the historical message record
and the live rule record in one reference-set certificate at the same
anchor.

Rule updates keep the Manifest, deployment and object identity fixed.
New state and any changed state must fit the current ceiling. A
previously admitted byte-identical state can transfer above the new
ceiling. A CKB burn can consume that state through its verified
custody gate. Stem uses the authenticated current CKB ceiling at
its committed anchor.

The code commitment is the CKB hash of
`"Stem Metatype ceiling route\0" || codes`. In order, the five
32-byte hashes identify the Stem escrow, CKB dispatcher, CKB gate,
CKB mailbox and CKB ceiling adapter. They bind the grandfathering
path for a previously admitted state arriving from the other chain.

An oversized CKB import supplies the 160-byte code preimage in
action 1 Proof Data of the ceiling Group Witness. The message and
seal fields are empty. The adapter verifies the physical dispatcher,
actual adapter table, gate and mailbox. It reconstructs the exact
Stem escrow Lock named by the authenticated import. That Lock
committed the original state through the native group.

For an oversized Stem return with changed bytes compared with the
escrowed object, the ceiling Type Group's Proof Data concatenates
the code preimage, the complete 149-byte physical CKB wrapper Script,
and its bounded adapter table. Stem matches the actual consumed
escrow Lock and pinned wrapper hash, verifies the table's content
hash, and checks the limit adapter and custody configuration. The
independent escrow Lock authenticates the exact returned state
through the pinned CKB gate and mailbox. This proves that the
grandfathered state passed the original ceiling rules on its
authoritative chain. A local edit cannot use the custody proof.

A state admitted before a reduction can therefore complete an
in-flight import or return. An edit above the current ceiling
fails even if its byte length stays constant. A revision that fits
the lower ceiling can resume normal editing. The cross-chain
exceptions use the existing CKB and Stem mailbox trust models.
Arbitrary policies that depend on intermediate state still need
their own replayable validation witness.

### Rotating object approval and freeze

An independent approval Type Script binds the full object Script
hash and the full Type hash of a CKB approval registry. Its Manifest
entry has empty state. Every approved transition retains the exact
approval Script alongside its bound object. It adds an approval role
to the object's existing ownership and Type Group checks.

The registry uses a CKB Type ID and a persistent owner Lock. Its
41-byte data contains a little-endian u64 generation, a 32-byte
Ed25519 public key and one byte of freeze state. Zero is active
and one is frozen. Creation uses generation zero. Updates require
the same Lock, the exact next generation, a valid nonweak key and
a change to the key or freeze state. The registry always recreates
its Type ID. The consumed owner Lock authorizes updates
independently of the approval key.

CKB resolves the actual typed Cell Dep. Stem resolves a guardian-
authenticated typed live reference at its committed CKB anchor.
A historical inclusion cannot provide a current record. The registry
can rotate, freeze or thaw while an object is in custody, with the
same object and approval Script identities. CKB enforces the current
record after the previous dependency is spent. Stem uses the
generation authenticated at its committed anchor, so activation there
depends on confirmation and anchor advancement. The owner Lock can
freeze every object using that registry.

The 64-byte Ed25519 signature covers the CKB hash of
`"Stem Metatype rotating approval\0" || network || deployment ||
approval_script_hash || generation_le64 || public_key || freeze_state ||
transaction_id`.
CKB uses its genesis hash, physical Metatype Type hash and raw
transaction hash. Stem uses its network ID, a zero deployment
field and its transaction ID, including the attached L1 certificate.
An approval is collected after all committed transaction data and
L1 attestations are fixed. CKB carries it as the seal in action 1
of the group witness, with empty message and proof. Stem carries
it in the native Type Group's Proof Data before Lock signing.
Issuance, transfer, import, burn, export and release each require
an active record and a seal for that exact transaction. A freeze
blocks these operations, including release of an already burned object.
The native escrow and the unconsumed return message remain available
until a confirmed thaw and a fresh seal permit release. The original
lock, approval and custody checks remain mandatory.

### Append-only revisions

An independent Type Script can bind an object's full identity
Script hash as its 32-byte args and require append-only updates.
Its Manifest entry has empty state. The object identity entry
continues to hold the canonical revision and content. Both hosts
use the same predicate. A successor either preserves the exact
encoded state or increases the revision, preserves the MIME type
and strictly extends the content byte prefix. The rule inspects
the decoded content, since the canonical length fields change
when content grows.

Native execution requires one output in the group with the bound
identity. An output-only group requires revision zero. Once the
rule is present, the input group requires a valid successor and
rejects removal. On CKB a local transfer checks the same relation
over the group's one input and one output. The authenticated import
and return boundaries check the canonical state on the represented
side. The dispatcher and mailbox gate bind those boundaries to the
full Manifest and exact message state.

For standalone use, the adapter accepts revision-zero creation
and a single-head transfer. Its deployment must pair it with an
appropriate identity constraint. The append-only devnet fixture
exercises the revisable custody route.

The relation is transitive. A sequence of strict byte-prefix
extensions preserves every earlier prefix, and ownership-only
steps preserve the exact state. Stem can check the relation from
the escrowed state to the authenticated return state after multiple
CKB revisions. The independent ownership, approval, and optional
length groups still execute. This predicate uses bounded state
reads and needs no additional cross-chain observation. A rule
requiring a particular intermediate event requires authentication
of the intervening changes.

### Signed workflow history

An independent workflow Type Script binds the 32-byte Stem network,
full object identity Script hash, data-addressed Stem escrow code hash,
and three distinct 32-byte Ed25519 public keys in step order. The keys
authorize request, review and completion independently. The object
uses the existing revisable encoding with MIME
`application/octet-stream` and 64 content bytes, consisting of a
32-byte subject and 32-byte workflow head. The workflow entry in the
Metatype Manifest has empty state.

Revision zero has an empty subject and the CKB hash of
`"Stem Metatype workflow start\0" || workflow Script hash`.
The configured Stem escrow accepts the object only after the request.
A nonzero subject chosen at the request stays fixed. For step
`n` from one through three, the new head and signed digest are the
CKB hash of the following concatenation.

```text
"Stem Metatype workflow step\0" || Stem network || workflow Script hash
    || previous head || subject || n_le_u64
```

An unchanged state carries no proof. A change supplies exactly the
concatenated 64-byte signatures for the intervening steps, in order.
Stem reads Proof Data from its current group. The Metatype adapter
reads a group witness with action 1, empty Message and Seal, and the
step proof. It verifies each CKB edit advances exactly one revision.
Import and return use the existing authenticated mailbox and carry
no additional CKB workflow proof. The initial request takes place
on Stem, so redemption replays at most two steps. The native group
requires the recreated head to equal the authenticated CKB state.
An early return preserves the current revision and requires only
the signatures for the steps completed since export.
The original owner and object approval Locks still apply. A proof
authorizes a specific workflow step independently of the relay
transaction and its recipient.

This profile demonstrates a bounded history with distinct semantic
obligations for each step. It authenticates the authorization chain
and the burned CKB state. Applications that also need exact CKB
heights or witness inclusion outside the normal CKB validation
assumption can verify the individual transaction and witness proofs
against authenticated CKB headers. The recovery command follows
the actual CKB OutPoint chain from burn to import, checks each
transaction and its witnesses against the supplied trusted CKB
anchor ancestry, and resolves shared definitions from the
transaction's actual dependencies. It checks the full original
Manifest and pinned mailbox as well as the signed path. A transfer
that preserves the state contributes no signature to Stem Proof
Data. The CLI requires retained CKB transactions and witnesses.
Additional workflow cycles and proof bytes enter the ordinary
Stem resource budget.

### Fixed Capacity royalty

An immutable or revisable object can bind an independent royalty
Type Script. Its 104-byte args contain the full object Script hash,
the Stem payee Lock hash, the CKB payee Lock hash and a positive
little-endian u64 amount in shannons. The Manifest entry has empty
state. Both implementations check the bound object's presence in
every royalty group Cell. A deployed rule requires a same-rule
successor and keeps its amount and payees fixed.

A transition that changes the object's Lock needs one fresh, untyped
payment Cell to the corresponding payee with at least the required
Capacity. Its 32-byte Cell Data is the CKB hash of
`"Stem Metatype royalty payment\0" || scope || royalty_script_hash`.
Stem uses the zero scope. CKB uses the full physical deployment Type
hash. Within the transaction this identifies a payment for exactly
one royalty group in one deployment. The proof of payment is the
actual output. The check is bounded to 128 outputs. The payment
Cell also needs enough Capacity to satisfy the destination ledger's
occupied Capacity rule.

Issuance and same-Lock updates require no royalty. A Stem export or
release changes the Lock, so each pays on Stem. CKB import and burn
use the already authenticated custody boundaries. The portable
adapter charges on CKB transfers between Locks. The amount is a
fixed protocol parameter for the individual rule. Payment does not
depend on a sale, an exchange rate or the identity of the person
controlling a Lock.

### Cap-bound fungible UDT

The native UDT args are the issuer Lock hash and series hash,
followed by the amount-cap Type code hash and a positive u128
ceiling. The UDT derives the complete required Data-addressed
cap Script from its own Script hash and the ceiling. It checks
membership on each native input and output in its group. The
separate cap Script uses args `asset Script hash || ceiling`
and requires each of its members to carry that asset with a
positive amount at or below the ceiling. The CKB adapters apply
both checks with the same logical identities. Each Cell holds its
own u128 amount as the designated Script state.

The native escrow checks the exact Manifest of every input and
output with the source asset identity. The CKB gate checks every
representation using the pinned physical Type. Stem export moves
the specified amount from circulating holdings into escrow while
preserving total escrow Capacity. Its nonce derives from the
first consumed escrow input. The fungible Intercell payload
is `Manifest ID || recipient Lock hash || positive u128 amount`.
CKB import requires zero inputs in that deployment and output
amounts summing to the authenticated issuance, all at that recipient.
Regular CKB transfers preserve the total. A decrease requires the
configured mailbox, which checks the exact burn difference.
An increase with existing wrapped inputs is invalid. Checked
arithmetic covers both sides before every virtual group executes.

Users can burn a portion of the CKB supply and keep the rest in
Cells with the same full policy. The mailbox advances its outgoing
nonce, and Stem releases the authenticated difference only to the
specified Lock with the original complete Type Script set. A partial
release retains the corresponding native principal in escrow. A
full release retains the escrow Capacity in untyped empty Cells.
Every individual output on either chain also satisfies the cap.
The two chains use their existing finality, anchor and replay
verification paths.

Two independent fungible deployments can settle atomically at
each stage. A Stem export or return carries one Intercell action
for each consumed escrow Lock group. CKB resolves each physical
Type and mailbox separately. Its import can verify distinct outbox
paths against one checkpoint, and its burn can produce two mailbox
outputs in one transaction. One reference-set certificate can
authenticate both outputs at their common CKB anchor. Any failed
message or Type Group aborts the whole transaction on its chain.
The lanes retain independent Manifest, asset, amount and nonce
checks. Confirmation and relay still occur between the two chains.

### Multiple assets in one Cell

Native fungible Cells can carry a canonical amount map. Data consists
of a one-byte count from 2 to 8, followed by that many pairs of a full
32-byte asset Script hash and a little-endian u128 quantity.
Keys are strictly increasing by bytes. The decoder rejects duplicate keys
and maps with no positive quantity. Zero retains the Script's slot when
the asset contributes no principal. Each UDT and its independent cap group requires its
asset key to be present and every map key to match a Type Script carried
by the Cell. The same
asset Scripts can validate ordinary scalar Cells, enabling local
composition and disassembly without changing asset identity.

The multiasset custody profile pins the complete Script set and the CKB
hash of `"Stem Metatype amount keys\0" || ordered asset Script hashes`.
The Stem escrow checks every participating Cell's Manifest and exact
amount keys, then sums each asset independently with checked arithmetic.
On CKB, one Metatype Cell can contain all those assets. The canonical
Manifest stores each u128 under its asset Script and passes that state to
the existing UDT and cap adapters. The custody boundary compares
per-asset totals rather than a combined amount.

The Intercell payload is the Manifest ID and recipient Lock hash
followed by the canonical amount map of the export or burn deltas.
An ordinary CKB transfer preserves every asset total. On return each
asset's quantity must be nonincreasing, at least one must decrease,
and the message records every exact difference. The complete Manifest
and all its groups remain required even if a quantity reaches zero.
The deployment's custody scope lets its UDT and cap adapters validate
these zero slots. Standalone and single-asset custody keep requiring
positive quantities on each asset Cell. When the last amount is
returned, the CKB representation is consumed and the native escrow
keeps only its Capacity bond.

These profiles establish correspondence for the specified source
rules. Other asset classes, state transition rules and external dependencies
need their own adapter profiles and differential verification.
Source upgrades require an authorized settlement at the old
deployment and a new approved table. Available live dependencies,
the Jolteon checkpoint assumptions, and guardian validation of
canonical CKB anchors remain part of the trust boundary.

## Threat model

The protected assets are supply, object identity, custody, script
obligations and portable state validity. Untrusted actors include
transaction builders, relayers and dependency publishers. They
choose outputs, witnesses, dependency references and message
ordering. They can deploy a different application with its own
Type Script. Their authority over an established deployment is
limited by its full physical Script, original asset and code
commitments, and the participating owners' Locks.

| Boundary | Failure under test | Required control |
| --- | --- | --- |
| Physical Type to virtual groups | A constraint is omitted, duplicated, or split across manifests | Derive the union and membership from every matching real input and output. Run each logical group once. |
| Group to adapter | A similar Script substitutes its arguments, code, or source indices | Bind full encoding, scope and data-authenticated program. Verify closed and bounded canonical traversal. |
| Group witness to authorization | A builder swaps approvals or carries a seal to a different group, output, chain or raw transaction | Address rows by full deployment and group. Compare actual recipient and content. Verify the genesis dependency and strict Ed25519 seal over the domain-separated digest. |
| Existing state to output state | A transfer removes a freeze, approval, identity or supply rule | Run all consumed groups and all created groups. Original policy decides whether retirement or migration is authorized. Revisable custody rejects rollback, same-revision edits and substituting stale data at the return boundary. |
| Shared definition to Manifest | Missing or substituted preimage, nesting, ambiguous state alignment or excessive expansion | Match the actual Cell Dep data hash, accept only canonical definitions with empty states, verify exact state cardinality and meter expanded bytes. Keep republishable preimages. |
| Rule record to object | A relayer replaces it with a looser rule, or spends the published copy | Resolve the pinned data hash in actual dependencies on both chains. Reject changed bytes and oversized states. Keep the original bytes available for publication at a new OutPoint. |
| Live ceiling | An updater forks the registry or a transfer uses grandfathering to create or rewrite an oversized state | Enforce Type ID and a same-Lock successor. Apply the current ceiling to changed states and check the proven custody code path for oversized cross-chain settlement. |
| Rotating approval and freeze | A spender uses a superseded key or record, or supplies a valid signature while frozen | Require an active current registry, an exact-generation strict Ed25519 seal and the same object and approval identities. Keep the CKB registry Lock responsible for rotation and freezes. |
| Append-only revision | An approval authorizes a rewrite, truncation, MIME change, or removal of the constraint | Bind the full identity Script, require a valid successor, and compare the exact revision relationship and decoded content prefix on both chains. |
| Signed workflow history | A return omits a review or substitutes, reorders or replays approval from another subject or chain | Verify each role's signature over the preceding head and exact step. Bind the fully replayed head to the authenticated return state. |
| Object royalty | A transfer omits payment, reuses a marker from another group or deployment, or replaces the rule | Require a fresh matching untyped output with sufficient Capacity and the same royalty Script on the successor. Run the original rule on Stem custody Lock changes. |
| Published evidence to live state | Old witness data is missing, or a historical proof is treated as live | Keep state in the referencing Cell and use authenticated context that states what was proven at the pinned anchor. |
| Asset map to custody | A transfer reassigns quantities between asset identities while preserving their combined total | Pin the complete Script set and ordered asset keys. Authenticate and compare every quantity independently at both boundaries. |
| Relay to escrow | A replay, another CKB domain, or a substitute manifest releases unrelated collateral | Pin both networks, asset and deployment. Use authenticated finality and durable per-lane consumption. |
| Guest and dependency bounds | Recursive execution, malformed lengths, or excessive combinations consume resources | Use bounded lists and byte reads, immutable reviewed adapters, sequential spawn and CKB cycles. Reject unknown dependencies and failed child results. |

The CKB contract platform meters cycles, process count and memory.
Deployment limits also bound the number of Cells, groups, adapter
entries and bytes scanned. CKB occupied Capacity includes the
physical script args and live Cell Data. Witness costs affect the
transaction's byte size and verification budget. The bidirectional
profile relies on the CKB observation and anchor assumptions of the
existing Intercell ingress until a stronger source verifier exists.

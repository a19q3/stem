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
immutable custody profile carries a native Polytype object from
Stem to CKB Metatype and back. Both profiles can store repeated
Script sets in a shared dependency. [Implementation status](STEM-EVOLUTION.md)

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
| Custody policy | mailbox code data hash, mailbox Type ID, gate code data hash, full source Manifest ID and original data-bearing Script hash |
| Adapter argument | full physical Type hash, one-byte boundary, u16 length and complete logical Script, input and output index lists, then optional group witness fields |
| Index list | one-byte count and strictly ascending absolute u16 transaction indices |

Adapter table records are ordered by logical code hash and addressing
mode. The one-byte custody tag is zero for a standalone deployment
with no following bytes and one for the five-field custody policy.
The dispatcher derives the boundary value from the authenticated
deployment and actual input/output cardinality. Boundary values
are 0 for standalone, 1 for custody transfer, 2 for CKB import and
3 for return to Stem. The dispatcher passes the argument as one hex C string. The
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
spawning any adapter. Each executing VM reads its actual
dependencies and caches the definition bytes locally. CKB cycles
meter that work. Witnesses carry current transaction material under
a separate size limit.

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

The immutable custody profile pins the original complete Script set,
an explicit data-bearing Script, the CKB deployment, and the
Intercell checkpoint and mailbox. The same escrow and mailbox gate
ELFs serve each compatible deployment. The supported object pair
uses independent adapters for source identity and transfer approval.
A route must pin a reviewed adapter for every source Script.
The caller verifies the configured deployment and its committed
table when choosing the Stem escrow Lock.

The source Manifest ID hashes the ordered sequence of complete
Molecule Script hashes. The native escrow derives it from actual
Type Scripts on **both sides** of the transaction. On CKB, the
same Scripts and arguments are present in the Manifest. The one
designated Script carries the exact source Cell Data and all other
entries have empty state. The immutable profile accepts only
Data-addressed source Scripts. It also rejects multiple physical
heads within one lane transition and requires identical Manifest
and state in the complete logical view on a CKB ownership transfer.
The checkpoint and mailbox commit the logical Manifest ID and
original Cell Data. Both the gate and the dispatcher resolve shared
CKB definitions against the actual dependency hashes.

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

The bridge outcome applies to the immutable profile. It does
not establish equivalence for arbitrary legacy binary contracts,
mutable Type Groups, fungible partial redemption, or unfunded
external state. Other asset classes require an explicit adapter
profile and differential verification of every source rule.
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
| Existing state to output state | A transfer removes a freeze, approval, identity or supply rule | Run all consumed groups and all created groups. Original policy decides whether retirement or migration is authorized. |
| Shared definition to Manifest | Missing or substituted preimage, nesting, ambiguous state alignment or excessive expansion | Match the actual Cell Dep data hash, accept only canonical definitions with empty states, verify exact state cardinality and meter expanded bytes. Keep republishable preimages. |
| Published evidence to live state | Old witness data is missing, or a historical proof is treated as live | Keep state in the referencing Cell and use authenticated context that states what was proven at the pinned anchor. |
| Relay to escrow | A replay, another CKB domain, or a substitute manifest releases unrelated collateral | Pin both networks, asset and deployment. Use authenticated finality and durable per-lane consumption. |
| Guest and dependency bounds | Recursive execution, malformed lengths, or excessive combinations consume resources | Use bounded lists and byte reads, immutable reviewed adapters, sequential spawn and CKB cycles. Reject unknown dependencies and failed child results. |

The CKB contract platform meters cycles, process count and memory.
Deployment limits also bound the number of Cells, groups, adapter
entries and bytes scanned. CKB occupied Capacity includes the
physical script args and live Cell Data. Witness costs affect the
transaction's byte size and verification budget. The bidirectional
profile relies on the CKB observation and anchor assumptions of the
existing Intercell ingress until a stronger source verifier exists.

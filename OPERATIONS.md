# Stem operations

## Build

Use the pinned Rust toolchain, the `riscv64imac-unknown-none-elf` Rust target, Clang and LLD, OpenSSL development headers, Go, Python and the Python `cryptography` package. Local L1 tests require a CKB executable with the IntegrationTest RPC module.

```sh
cargo build --locked --release -p stem
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --manifest-path contracts/Cargo.toml --release \
  --target riscv64imac-unknown-none-elf
mkdir -p target/stem-tools target/stem-runs
GOBIN="$PWD/target/stem-tools" go install github.com/cometbft/cometbft/cmd/cometbft@v0.38.26
go -C tools/proof build -mod=readonly -o ../../target/stem-tools/stem-proof .
go build -o target/stem-tools/stem-loadgen tools/loadgen/main.go
```

## Start a native replica

The Stem genesis JSON requires `chain_id`, a 32-byte `network_id`, `genesis_time_ms`, storage-bond terms, and explicit byte, cycle, read and write limits. A stake-backed genesis supplies `stake.trust` and `stake.observations` from CKB guardians, a signing window, and snapshot limits. The trust file contains the complete CKB JSON `registry_type`, guardian set, snapshot context and Stem election limits. Genesis derives the Registry hash, minimum bond and epoch length from that Script and checks the signed observations. The `development_validators` and `development_cells` fields are for fixture networks. A bridged spec pins the CKB genesis and anchor, initial vault OutPoints, wrapped code, payout Lock templates, and the bridge guardian set. Each lane supplies the complete CKB JSON `vault_type`.

Compute `stem genesis-hash --genesis stem.json` and verify that every replica
uses the same specification. The configuration file lists `peers` as
`{key, address}` entries in ascending Ed25519 public-key order, as well as
`view_timeout_ms`, `proposal_interval_ms` and optional `vm_workers`. A
replica uses one private 32-byte Ed25519 seed.

```sh
stem run --initialize --home /srv/stem/app --genesis /srv/stem/stem.json \
  --configuration /srv/stem/replicas.json --key-file /srv/stem/validator.key
```

On subsequent starts omit `--initialize`. Keep the private home, key and
voting journal together. Ordinary replicas retain the latest 256 blocks by
default, including their Headers, execution records, proposal bodies and
finality records. Choose `--history-window` when initializing to cover both
the Header Dep and stake signing horizons. Use `--archive` at initialization
to serve older bodies and proofs. Restarts use the same retention policy.
Both roles keep verified committee handoffs and the active epoch anchor.
Keep a reachable archive for replicas recovering from beyond the ordinary
window. Create a fresh development home when replacing an older research
build whose database predates ordered pruning.

## Operate a replica under systemd

Install the `ops/systemd/stem@.service` unit for the service user and derive
`node.env` from the example. Initialize the private home once with the same
key and history role. Set `ROLE_ARGS=--archive` for an archive replica.

```sh
instance=$(systemd-escape --path /srv/stem)
systemctl --user start "stem@${instance}.service"
journalctl --user -u "stem@${instance}.service"
```

Each validator key requires a single signing process. Back up the Stem
specification, guardian trust, and validator key and journal independently
of disposable observer snapshots. The peer directory is static and the
transport is intended for a private network.

## Exercise the native Jolteon path

The fixture starts ordinary replicas and one archive using `stem run`. It generates disposable keys for a private loopback network.

```sh
python3 tools/jolteon_localnet.py \
  --binary target/release/stem \
  --lock target/stem-contracts/riscv64imac-unknown-none-elf/release/stem-lock \
  --output /tmp/stem-jolteon-16 --validators 16 --transactions 64 \
  --offered 32 --cadence 200 --timeout 850 --offline 2
target/release/stem jolteon-proof --genesis /tmp/stem-jolteon-16/genesis.json \
  --proof /tmp/stem-jolteon-16/two-chain.json
```

The fixture checks consecutive failed Leaders, watches surviving weight finalize payments, restarts stopped replicas and checks the execution Header at a common height. It retains the public genesis, signed workload, execution proof and replica diagnostics. For the nominal scale sweep use `--offline 0 --transactions 32 --offered 16 --cadence 300 --timeout 2500` and separate output and port ranges for each size. The `tools/comet_matrix_localnet.py` comparator uses the same fixture against a pinned CometBFT application build. Its `--comet` argument takes the CometBFT executable.

Use `--alternate-binary` with a second Stem executable to run the
odd-numbered replicas on that build. The report records the binary
assigned to each replica. Restarts use the same binary and database
as the original replica. The compact Patricia layout requires a
fresh database directory. Canonical state images can initialize that
directory through the normal authenticated snapshot workflow.

Add `--require-relay-retry` with `--offline 2` to submit the first
payment while both initial relay targets are stopped. The fixture waits
for a single local submission to become visible on every live replica
and checks the source retry counter before sending the remaining
payments. The admission reply reports an in-memory entry. Keep the
signed transaction until finality so the client can resubmit after
failure or a process restart.

Repeated submissions from a Rust client can use
`Client::new(address, timeout).with_submission_reuse()`. The SDK keeps
up to four idle sockets per client and drops them from the pool before
reuse if the last reply is at least 500 ms old. Each socket has one
request in flight. A transport failure is returned to the caller, who
can check finality or submit the same signed transaction on a new
socket. Status and proof queries continue to open their own sockets.

Use synchronized realtime clocks on validators. A Jolteon vote authenticates
an execution timestamp within 30 seconds behind or 1 second ahead of the
local clock. The `last_untimely_view` status field shows the latest rejection.
Use `tools/jolteon_clock_localnet.py --binary <stem> --lock <stem-lock>
--output <new-directory> --offset-seconds 120` to test a fast leader, then
repeat with `--offset-seconds -120` and a new output directory. The Linux
fixture perturbs realtime for the first leader and leaves the pacemaker clock
running normally. Its `.so` is built from a local C fixture.

Exercise long-gap replay with `tools/jolteon_localnet.py --offline 1
--offline-blocks 220 --cadence 20 --timeout 250 --recovery-seconds 30
--history-window 64 --check-pruning`
along with the binary, lock and output settings above. The restarted replica
must report a positive `replayed_blocks` count and agree with the survivors
on the execution Header. Recovery peers must have retained the missing
payloads and finality certificates.

To check the CKB relay path, run the 128-replica Jolteon fixture using
`--validators 128 --offline 0 --transactions 32 --offered 16 --cadence 300
--timeout 2500`. It leaves a signed proof and an archive to feed to the
two-node CKB test. Build `jolteon-checkpoint` for RISC-V as shown below.

```sh
python3 tools/jolteon_ckb_relay_localnet.py --binary /path/to/stem --l1 /path/to/ckb \
  --direct-checkpoint /path/to/jolteon-checkpoint --stem-run /tmp/stem-jolteon-128 \
  --output /tmp/stem-jolteon-ckb-relay --l1-port 22900 --relay-port 22910
```

The fixture checks both nodes' default transaction pool settings. It submits
the checkpoint to one node and waits for the second node to admit it through
CKB P2P and include it in a block. The IntegrationTest RPC is used to generate
devnet blocks. Setup blocks are imported with normal verification.

Add `--checkpoint-proofs 3` to the Stem fixture for the competing
publisher run. Keep its archive and validator fixture files. The CKB
fixture reopens the stopped archive to serve the exact proofs. It uses
two isolated CKB pools for the competing spends, then connects them
and verifies canonical adoption and P2P relay of the next publication.

```sh
python3 tools/jolteon_ckb_relay_localnet.py --binary /path/to/stem --l1 /path/to/ckb \
  --direct-checkpoint /path/to/jolteon-checkpoint --stem-run /tmp/stem-jolteon-128 \
  --output /tmp/stem-jolteon-ckb-contention --l1-port 22900 --relay-port 22910 \
  --background-checkpoints 16 --ckb-block-cycles 3500000000
```

Use a fresh output directory for each trial. Set
`--background-checkpoints 0` for the single-update control, or
`4` or `8` for intermediate bursts. Omitting `--ckb-block-cycles`
uses the devnet's consensus setting. Each background transaction
creates an independent checkpoint Type ID, and each proof still
runs under the stock CKB transaction pool policy. The reports retain
pending-pool observations and both nodes' witness inclusion proofs.

Export a snapshot from a stopped replica after it finalizes the exact Header
you intend to pin. Independently verify the canonical, sufficiently confirmed
CKB checkpoint Cell and obtain its Stem Header ID. Use that ID as
`--trusted-header` when installing the snapshot on a fresh observer.

```sh
stem jolteon-snapshot export --home /path/to/stopped-archive --genesis stem.json \
  --snapshot state.bin --certificate finality.json
stem jolteon-snapshot restore --home /path/to/new-observer --genesis stem.json \
  --snapshot state.bin --certificate finality.json --key-file observer.key \
  --trusted-header "$confirmed_stem_header_id"
```

Start the observer normally without `--initialize`. The restored journal
permanently disables voting and survives later handoffs. Its transaction
counter starts at the snapshot. Signing nodes recover with their retained
vote journals and certified blocks. The CKB-backed stake fixture's
`--snapshot-restore --observer-extra-lag 220` exercises long-gap replay,
checkpoint-pinned installation and live follow-up in the same run.

To measure the portable verifier with the signed proofs captured in the matrix, build and run the two probes.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --target-dir target/stem-contracts \
  --example jolteon-finality-cost
cargo build --locked --release --manifest-path vendor/ckb-vm/Cargo.toml \
  --features asm --target-dir target/jolteon-vm-runner --example ckb-vm-runner
python3 tools/jolteon_finality_cost.py \
  --runner target/jolteon-vm-runner/release/examples/ckb-vm-runner \
  --elf target/stem-contracts/riscv64imac-unknown-none-elf/release/examples/jolteon-finality-cost \
  --archive benchmarks/2026-11-25-stem-jolteon-runtime.json.xz
```

To exercise CKB-backed committee handoffs and native checkpoint publication, use
a CKB executable with IntegrationTest RPC enabled. Configure the peer directory
with the participating validators and observers before starting the fixture.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --target-dir target/stem-contracts \
  --bin stake-registry --bin jolteon-checkpoint
contracts_dir=target/stem-contracts/riscv64imac-unknown-none-elf/release
python3 tools/jolteon_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --output /tmp/stem-jolteon-epochs \
  --stake-registry "$contracts_dir/stake-registry" --direct-checkpoint "$contracts_dir/jolteon-checkpoint" \
  --publish-checkpoint --replace
```

Drop `--replace` to test an overlapping join followed by an exit. The
runner saves CKB transactions, guardian observations, boundary witnesses
and execution proofs. `stem jolteon-proof --handoff <file>` takes preceding
handoff files in chronological order. Use `--terminal` for a boundary
proof. `stem checkpoint jolteon-bundle` reads an archive and constructs a
CKB witness with the previous frontier, history suffix and finality proof.

### Exercise a full funded committee handoff

Build `stake-registry` and `jolteon-checkpoint` as above. The
wide fixture funds every validator's Stake Cell on CKB, verifies
guardian observations for genesis and epoch 1, and starts one
extra process for the incoming key. It pauses a continuing voter
before the snapshot, restarts it after the committee changes,
and publishes the old, boundary and successor Headers on CKB.

```sh
python3 tools/jolteon_wide_epoch_localnet.py \
  --binary /path/to/stem --l1 /path/to/ckb \
  --stake-registry "$contracts_dir/stake-registry" --direct-checkpoint "$contracts_dir/jolteon-checkpoint" \
  --validators 128 --port 29630 --l1-port 29780 --output /tmp/stem-wide-handoff \
  --blocks-per-epoch 40 --signing-window 30 --ckb-block-cycles 3500000000
```

Use a fresh output directory and 129 available consecutive
Stem ports. The CKB RPC and P2P ports must fall outside that range.
The registration phase confirms each Registry update in order,
and reports the transaction hashes as it progresses. Use
`--validators 4` for a smaller devnet run with the same proof checks.
The fixture uses temporary test keys.

To include a funded cross-epoch Intercell delivery, add these flags to
the wide fixture and use a new output directory.

```sh
--intercell-mailbox "$contracts_dir/intercell-mailbox" \
--vault "$contracts_dir/vault-type" --deposit-lock "$contracts_dir/deposit-lock" \
--native-lock "$contracts_dir/stem-lock" --wrapped "$contracts_dir/bridge-udt"
```

The fixture mints against real CKB reserves and saves an Outbox proof
before the boundary. After the new committee finalizes another send,
it publishes a later checkpoint and delivers the original message
using the checkpoint history path. The same transaction body with a
fresh Outbox proof provides a cycle and size control.

After the funded fixture exits, replay its original signed epoch
boundary and successors into two fresh CKB nodes. The relay fixture
also submits the recorded historical Intercell delivery if present.

```sh
python3 tools/jolteon_ckb_epoch_relay_localnet.py \
  --l1 /path/to/ckb --stem-run /tmp/stem-wide-handoff \
  --output /tmp/stem-wide-epoch-p2p \
  --source-port 30200 --l1-port 30210 --relay-port 30220 \
  --ckb-block-cycles 3500000000
```

Use the same block cycle budget as the funded fixture. The runner
opens a copy of its stopped CKB devnet and independently imports
the verified chain prefix into both fresh nodes through the old
committee's checkpoint. That copy stays disconnected from the
test peers. The runner sends the signed boundary and successor to
the origin only. The receiving node accepts each over P2P before
mining. The final report compares the original and new inclusion
proofs, the committed transaction bytes, and the cycles recorded
by both CKB peers.

### Run a sparse CKB checkpoint publisher

Run one poll with `stem checkpoint publish --config publisher.json`. Schedule that
command under a timer. It publishes at most one CKB transaction per poll, and
prints its status as JSON, such as `idle`, `submitted`, `in_pool`,
`awaiting_confirmations` or `confirmed`. Keep polling through confirmation. Use `--at-least-height H` while
a withdrawal, Intercell delivery or retirement needs a more recent checkpoint.
The publisher targets H exactly if H is finalized and ahead of the last
published checkpoint. Missing handoffs through the chosen target are relayed
first, one at a time, with the incumbent roster and their exact boundary
Headers. The first poll can initialize the checkpoint as soon as a valid
finalized Header or the first handoff exists.

The configuration uses the following fields.

| Field | Value |
| --- | --- |
| `l1`, `replica` | CKB RPC URL with Chain, Pool and Experiment methods enabled, and Stem archive replica socket address |
| `archive_db`, `index` | Stem archive RocksDB path and a separate path for the rebuildable proof index |
| `journal` | Exclusive, private RocksDB directory owned by the publisher |
| `policy`, `initial_roster` | Hex Type Script arguments and path to a JSON hex string containing the genesis committee record |
| `checkpoint_code`, `checkpoint_code_hash` | CKB code OutPoint and its pinned Data2 code hash |
| `seed`, `fee_dep_group` | The CKB seed OutPoint pinned by the Type ID and the CKB secp256k1 Sighash dep-group OutPoint |
| `fee_key` | Path to a private 32-byte CKB Sighash key. The initial publisher uses the seed owner key. |
| `update_funding` | Optional plain CKB OutPoint locked to this publisher's fee key. Set it for a publisher joining an existing checkpoint. |
| `auto_follow` | Optional boolean. With CKB Indexer enabled, discover confirmed successors after the journaled OutPoint is spent. Defaults to false. |
| `checkpoint_capacity`, `fee` | Checkpoint capacity and fee per publication in shannons |
| `interval_seconds`, `confirmations` | Routine interval in finalized Stem seconds and the number of CKB blocks required after inclusion |

Each OutPoint uses CKB RPC JSON with `tx_hash` and a hex `index`. The
seed must be a plain CKB funding Cell. Each publication retains the
same checkpoint capacity and returns fee change to that key. Budget
the seed for the initial checkpoint, change occupied capacity, and
planned fees. Keep the journal across process restarts. A shallow fork
can retry the same signed transaction. A reorganized confirmed
checkpoint anchor halts the journal. For automatically following another
publisher, enable `Indexer` in the CKB node's RPC modules and set
`"auto_follow": true` in the publisher configuration. When the tracked
OutPoint has been spent, the next poll searches the index by the exact
checkpoint Type Script and verifies each confirmed successor in spend
order. A fresh journal starts at the transaction consuming the pinned seed.

The poll returns `awaiting_successor` while confirmation or indexing
is pending and `adopted` after saving up to 64 verified updates.
Repeat the poll to process longer pending history. For an on-demand
target, keep polling with `--at-least-height` until the needed
checkpoint is available.

Manual recovery accepts confirmed transaction hashes in spend order.

```sh
stem checkpoint publish --config publisher-b.json \
  --adopt "$first_missing_tx_hash" --adopt "$next_missing_tx_hash"
stem checkpoint publish --config publisher-b.json --at-least-height "$target_height"
```

Each invocation accepts up to 64 updates. It checks the exact
predecessor OutPoint, pinned Lock and Type Script, native finality
and committee handoff witnesses, and the configured CKB confirmation
depth. The full list is recorded atomically. Successful adoption
resolves a stale pending publication. A publisher uses its own tracked
change or its configured funding Cell. Use independent journals,
fee keys and proof indexes for multiple publishers.

Use the stake runner from the preceding example with `--exercise-publisher
--replace` in place of `--publish-checkpoint --replace` and with a new
output directory. It uses actual CKB blocks and records each fresh CLI
invocation, canonical witness inclusion proofs and the response to both
shallow and confirmed-anchor reorganizations. Use
`--exercise-publisher-handoff --replace` to test two fee wallets,
a competing spend, and catch-up from a fresh journal. Add
`--auto-follow` to exercise the Indexer path and dependent
successors in one CKB block. Add `--auto-follow-long` to cross
the 64-update limit with 65 confirmed successors.

### Fund CKB checkpoint fees

Build and deploy the `checkpoint-fee-fund` Data2 contract on CKB.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --target-dir target/stem-fund-contracts \
  --bin checkpoint-fee-fund
```

Use `stem checkpoint fund lock --request lock.json`. The request fields
are `code_hash`, `checkpoint_code_hash`, `checkpoint_type_id`, `network`,
`ckb_genesis`, `fee_cap` in shannons and `interval_ms` in Stem consensus
milliseconds. Set the returned Lock as the PoW Security Fund recipient
and pin its Data2 hash in `bridge.checkpoint_fee_fund_code` at genesis.
The original four-account accrual and vault claim flow supplies the L1
fund pot. Each payout must independently cover the Lock's 185 CKB
occupied capacity. Consolidate two or more same-Lock pots into one,
preserving all principal and adding a separate fee input.

After bootstrapping the checkpoint with a wallet, pass
`--fee-fund fund.json` to `stem checkpoint publish` for an eligible
ordinary checkpoint or mandatory committee handoff. The file contains
the pot's `out_point` and its code OutPoint in `code`. The publisher
returns the submitted transaction and `fund_change`. Poll through
confirmation using the same journal. The funding flag applies to the
chosen new update, and the journal carries the decision through retries.
After confirmation, a normal poll uses the separate wallet. For a later
funded update, provide the confirmed successor OutPoint.

A relayer with a Jolteon bundle can also build an independent transaction
using `stem checkpoint fund update --l1-rpc URL --request update.json`,
and submit with `--submit`. The request fields are `checkpoint`,
`checkpoint_code`, `fund`, `fund_code`, `fee` in shannons and the
bundle's hex `witness`. OutPoints use CKB RPC JSON. Check canonical CKB
inclusion and confirmation depth before treating the fund change as
settled. When managing a checkpoint with a publisher journal, use its
fund option so the journal advances with the sponsored transaction.

Add `--fee-funds --checkpoint-fee-fund PATH` to the Jolteon bridge fixture
to exercise accounting, actual payout, merge and funded publication on
CKB. Add `--checkpoint-fee-fund PATH --replace` to the stake fixture to
exercise the funded committee handoff and a subsequent wallet publication.

To test permissionless payouts and stake liability, build the CKB consumers
for the native checkpoint. The default contract build selects Jolteon checkpoint verification. A new deployment pins the corresponding CKB code hash and Type ID.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --target-dir target/stem-jolteon-contracts \
  --bin jolteon-checkpoint --bin stake-registry --bin vault-type \
  --bin deposit-lock --bin xudt-gate
native_dir=target/stem-jolteon-contracts/riscv64imac-unknown-none-elf/release
programs_dir=target/stem-contracts/riscv64imac-unknown-none-elf/release
python3 tools/jolteon_bridge_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --output /tmp/stem-jolteon-bridge \
  --vault "$native_dir/vault-type" --deposit-lock "$native_dir/deposit-lock" \
  --direct-checkpoint "$native_dir/jolteon-checkpoint" \
  --native-lock "$programs_dir/stem-lock" --wrapped "$programs_dir/bridge-udt" \
  --xudt /path/to/xudt_rce --xudt-gate "$native_dir/xudt-gate"
python3 tools/jolteon_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --output /tmp/stem-jolteon-slash \
  --stake-registry "$native_dir/stake-registry" --direct-checkpoint "$native_dir/jolteon-checkpoint" \
  --publish-checkpoint --retire --slash --active-slash
```

The retirement fixture also accepts `--retire` alone for an unpenalized
exit. Adding `--slash --replace` exercises evidence from the genesis
committee after it has been replaced. The Jolteon client frame type 25
exposes the same read-only execution and SystemRoot queries used by the
application. For withdrawal reconciliation, pass `--commit` a JSON object
with `proof` (hex certificate bytes), `terminal` (boolean) and, for an
earlier burn Header, `history` (hex MMR inclusion proof). Pass `--handoffs`
the JSON array of preceding hex handoff proofs when the certificate is from
a successor committee. The optional CometBFT comparison arguments select
the frozen comparison verifier explicitly.

Verify actual RISC-V composition through the default runner with
`cargo test --locked --release --test native_store native_default_preserves_polytype_intent_lineage_and_keeper -- --ignored --nocapture`.
The Rust SDK exposes `stem::jolteon::Client` for admission, Status, Headers,
two-chain proofs, handoffs and native application queries. Verify a received
certificate against the configured genesis and prior handoffs. A certificate
for a later descendant requires proof of the requested Header's ancestry.

## Reproduce the CometBFT comparison

The frozen comparison source is commit `12744bf`. A fresh comparison build
from the current source exposes `stem comet-abci` with
`--features comet-benchmark`. Use CKB contracts from
`--no-default-features` for the CometBFT-specific Vault and Registry fixtures.
The matching systemd units are in `ops/comet-benchmark/systemd` for the
frozen executable.

```sh
cargo test --locked -p stem-ledger encoding_tests -- --nocapture
cargo test --locked -p stem-vm --test native -- --nocapture
STEM_TEST_COMET=target/stem-tools/cometbft \
  cargo test --locked --features comet-benchmark --test native_store four_node_native_finality_restart_and_app_hash \
  -- --ignored --nocapture
```

The backend can also run its imported upstream test suite with `cargo test --manifest-path vendor/ckb-vm/Cargo.toml --features detect-asm`. The Stem workspace pins its backend and definitions to the source in that directory.

The four-node test prints its evidence directory. It generates a fresh network, starts four validators, exercises signed payments and a two-Type-Script transfer, restarts a replica and joins a fifth node using remote snapshots. It also commits an independently signed two-asset trade with a sponsor and authenticates explicit and dynamically resolved Lineage successors on all replicas. The same harness verifies a Keeper wake with zero submitted transactions in its CometBFT block. It leaves the test configuration, logs, headers, commit certificates and lifecycle proofs in the evidence directory.

```sh
STEM_NATIVE_EVIDENCE=/path/to/printed/evidence \
  go -C tools/proof test -mod=readonly -run 'Test(NativeCommit|LineageCommand|KeeperWake|SponsoredLineage)Signatures' -v .
```

The benchmark uses the same four-node harness. Set `STEM_TEST_LOADGEN`, `STEM_TEST_LOAD_RATE`, `STEM_TEST_LOAD_COUNT` and `STEM_TEST_LOAD_KIND`. Workloads are `payment`, `single`, `double`, `multi` and `settlement`. Set `STEM_TEST_EVIDENCE_ROOT=target/stem-runs` to keep evidence on the workspace filesystem. Set `STEM_TEST_VALIDATORS` to vary the same-host benchmark from 4 to 32 replicas. Use `--release` for throughput characterization.

In benchmark mode, `STEM_TEST_BLOCK_CYCLES`, `STEM_TEST_BLOCK_TX_LIMIT`, `STEM_TEST_BLOCK_BYTES`, `STEM_TEST_CELL_READS`, `STEM_TEST_CELL_READ_BYTES` and `STEM_TEST_CELL_WRITE_BYTES` control the disposable genesis. `STEM_TEST_COMMIT_MS` and `STEM_TEST_MEMPOOL_SIZE` control CometBFT's configured commit wait and transaction count per mempool. The report distinguishes attempted submission, RPC admission, client dispatch drops and full-mempool replies. It reports separate Stem and CometBFT CPU, sampled RSS and process write counters. These counters measure operating-system writes at process granularity. Confirmation requires observing H+1 on every replica. `block-load.json` records observed transaction counts and charged cycles. See [matched run settings and outcomes](BENCHMARKS.md#local-pressure-and-replica-sweep).

```sh
cargo test --locked --release --test weighted_finality
cargo build --locked --manifest-path contracts/Cargo.toml --release \
  --target riscv64imac-unknown-none-elf --example certificate-cost
STEM_TEST_CERTIFICATE_ELF=contracts/target/riscv64imac-unknown-none-elf/release/examples/certificate-cost \
  cargo test --locked --release --test certificate_cost -- --nocapture
```

The certificate probe reports one weighted signer payload at 4, 16, 32, 64, 100 and 128 members. The optional ELF adds CKB-VM V2 cycle measurements for the same independently generated signatures. The ELF exercises signature verification with an authenticated committee input.

## Wallet and proofs

### Portable Strand proofs and snapshots

`stem checkpoint history` can write the same historical Header, MMR frontier and sibling hashes as a binary Strand file with `--strand-output history.strand`. The ordinary JSON response is also printed. Pass a file with the `.strand` suffix to the Intercell delivery command's `--history` option. Both inputs produce the deployed CKB witness format. A verifier must authenticate the expected history root separately.

`stem stake delegation-proof` accepts `--strand-output position.strand` for a single delegation principal or an absent position. Check it against the root from the authenticated Registry entry using `stem stake verify-delegation-proof --network NETWORK --root ROOT --proof position.strand`. The returned identity includes the registry, validator, owner and nonce. An authenticated zero principal proves absence at that root.

State snapshot files use the existing streaming layout. Its envelope, header suffix, record keys and record lengths are specified in the Strand schema. The node streams the records and authenticates the restored state against the supplied checkpoint. Existing snapshots and authenticated keys retain their byte representation. A four-validator archive produced a 384-byte history proof and the membership/absence fixtures produced 204-byte delegation proofs. [Archived public examples](evidence/2026-11-27-strand-proofs.json.xz)



For paired partial authorization, fund one Cell using the `stem-intent-lock` Data code hash and 52 bytes of arguments. The first 20 bytes are the owner's compressed-key Blake160, followed by a 32-byte locally unique salt. Choose the exact delivery Cell and resolve the common Cell and Header Deps before signing. Use `stem wallet intent sign` with the signing-context JSON and a draft whose selected input and delivery are at the same index. The signer supplies an exclusive `--valid-before-height` and `--max-fee` in shannons. A sponsor additionally supplies `--sponsor-max-debit` and puts plain change at index 0 and the exact sponsored pair at index 1. The solver builds a new draft with every pair and uses `stem wallet intent attach --intent first.json --intent second.json`. Collect any complete transaction signatures after attaching the terms. To cancel, consume the input with `stem wallet sign --intent-code /path/to/stem-intent-lock` and empty proof data for that Lock Group.

For Action-bearing pairs, every signer and the sponsor pass the same `--actions actions.json` to `wallet intent sign`. The solver supplies that file to `wallet intent attach` along with the signed pairs. The entries are ordered by role (`lock` then `type`) and raw Script hash, with one entry per identity. Within an entry, the order of Actions is signed. For example, the decoded `open` Action for a Type group is recorded as follows.

```json
[{"role":"type","script_hash":"0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","actions":[{"kind":7,"data":"0x6f70656e"}]}]
```

Use the actual full Script hash from the offered or delivered Cells. The signing fragment may omit other pairs. The solver must produce every group named by the manifest. When using a draft that already has its final indexed Group Messages, omit `--actions` and use a signing context that resolves all inputs referenced by the draft. Build and publish the current Intent Lock ELF and re-sign partial quotes using its Action-bound signature domain.

To enable Lineage lifecycle transitions in a fresh network, set `lineage_type` to the Data hash of the compiled `stem-lineage` RISC-V ELF and publish that program as a Cell Dep. Use `stem_ledger::lineage::id` and `marker` to build the Type Script. Register from an ordinary input at index 0 using group Action 0 with a 32-byte Executor Data hash. Use Action 1 for code replacement and Action 2 with empty data for retirement. Query `/stem/lineage` with a canonical `LineageRef` and `prove=true`. Decode the Strand registry `record`, its `proof`, the `cell` and `cell_proof`. Verify both paths with `LineageProof::verify` against the same authenticated Header. Save that Header's finality proof and both state paths to verify the version after the Head advances. Registration, Executor replacement and retirement use explicit OutPoints.

For dynamic commands, publish the registered Executor and the example `stem-lineage-lock` as Cell Deps. Prepare a head with `stem_ledger::lineage::lock` in head mode. Fund a separate plain CKB Cell with the same helper in fee mode. Use `Transaction::new_lineage_command` with the fee OutPoint, ordered target references, Executor Data hash and OutPoint, signed fee, execution budgets, application payload and exclusive expiry. Sign `Transaction::lineage_signing_digest` with the fee Cell owner's secp256k1 key and place its 65-byte seal at Group Witness index 0. Use the ordinary node broadcast interfaces. The client must authenticate the fee Cell and check the Executor and application payload before signing. Owners can also authorize exact Head transitions with a complete transaction signature.

The CLI builds and signs the same native command. The build request has
`fee_input`, `cell_deps`, `targets`, `executor`, `executor_dep`,
`valid_before_height`, `executor_cycles`, `validation_cycles`, `fee`
and hex `payload` fields. A target contains the hex `reference` and
`min_capacity`. `Latest` also accepts an optional `latest_bounds` array
`[min, max]`, inclusive at both ends. Order targets by raw ID bytes. The
signing context includes the authenticated fee and Executor Cells. Supply
a separate expected Executor hash and a local fee cap before signing.
Rebuild and re-sign pending Lineage commands when switching to the reference
wire format, including sponsored batches and Keeper reservations on a fresh network.

```sh
id=$(stem wallet lineage id --genesis stem-genesis.json --anchor anchor.json --output-index 0 | jq -r .)
stem wallet lineage reference --id "$id"             # Latest
stem wallet lineage reference --id "$id" --version 42 # Exact(42)
stem wallet lineage build --request command.json > draft.json
stem wallet lineage sign --genesis stem-genesis.json --transaction draft.json \
  --context cells.json --code stem-lineage-lock --key-file fee-key \
  --executor-hash "$EXPECTED_EXECUTOR" --max-fee "$FEE_CAP" > command.signed.json
```

For an ordinary exact spend of a Head or fee Cell, use
`stem wallet sign --lineage-code stem-lineage-lock` with the usual
transaction, Lock, context and key arguments.

Use `lineage::Outcome::decode` on the authenticated receipt protocol result. Position 0 always refers to fee change. On success each ordered target's output follows it. On a fee-only failure no target outputs are created. The four-node runner exports `lineage-commands.json` containing the real command results and the state and certificate evidence.

**Sponsored Lineage intent.** Use `stem wallet lineage actor --key-file KEY` to derive each business ID. Include `"business":{"actors":[…],"max_fee":2000}` with sorted actor IDs in the Lineage build request. Bind at least one target to an exact version. The business draft may omit `fee_input` and `fee`. Each party reviews the full business payload and uses `stem wallet lineage sign-business --genesis SPEC --transaction DRAFT --executor-hash HASH --key-file KEY`. The collector then runs `stem wallet lineage assemble --genesis SPEC --transaction DRAFT --fee-input OUTPOINT_JSON --fee SHANNONS --seal PARTY_A_JSON --seal PARTY_B_JSON`. Only the fee payer seals the assembled transaction with `stem wallet lineage sign`. For Keeper, set `execute_at` or `execute_after_ms` before collecting the business seals and reserve the fully authorized transaction.

**Atomic Lineage batch.** Build each unsigned business intent, then run the following command in the order all parties will review.

```sh
stem wallet lineage plan --genesis SPEC --member A.json --member B.json > plan.json
jq '.members[0]' plan.json > bound-a.json
jq '.members[1]' plan.json > bound-b.json
```

Each actor signs the appropriate bound draft with `wallet lineage sign-business --plan plan.json`. Assemble each command separately with its business seals and fee Cell. Each fee payer signs its assembled member with `wallet lineage sign --plan plan.json`. Combine the signed commands in the reviewed order.

```sh
stem wallet lineage batch --genesis SPEC --member signed-a.json --member signed-b.json > batch.json
```

Submit `batch.json` through the ordinary transaction interface. Publish signed members inside the batch so their plan can be verified at execution. Use the batch's transaction and receipt proof to authenticate the complete signed members. A member's output 0 is its sponsor's refund, and its other output indices follow the member's signed target order. Verify the resulting Live Cells and the registry Head at each member's own transaction ID. Use newly built drafts and seals when starting a network with the updated Lineage Intent encoding.

**Keeper.** Enable `keeper` in the genesis JSON of a Lineage-enabled network. The local time-enabled fixture uses `{"max_delay":64,"per_height":4,"reservation_fee":2000,"time":{"per_height":2,"max_delay_ms":60000}}`. Its timings and limits are experimental. Set either `execute_at` to a block height or `execute_after_ms` to an absolute Unix millisecond timestamp in the Lineage request, with an exclusive `valid_before_height`. Provide enough height slack for the parent Header to reach the timestamp and for competing reservations to clear.

The trigger is part of the signed Strand bytes. Rebuild and sign any pending development commands using the current schema. For an atomic batch, set the same trigger in each unsigned member before planning and signing. Its earliest member expiry applies to the batch. Include the fully signed command or batch hex bytes in an ordinary wallet build request under `"keeper":{"schedule":"0x…"}` and sign the carrier with its own funding inputs. The carrier pays the reservation charge and regular resource fee. The reserved command runs when selected by the committed queue.

**Keeper retry.** Before signing the Lineage request, add `"retry":{"max_attempts":3,"delay_blocks":8}`. Use the same trigger and retry policy in every batch member. Repeatable height jobs use the configured time slots and begin at or after the signed height. Budget three times the reservation charge in the carrier for this example, plus its regular resource fee. Keep enough exclusive height slack for all attempts. A failure that leaves more eligible attempts returns outcome 3, zero execution fee and a new `eligible_height` in the proof from `/stem/keeper-time`. A successful attempt charges the authorized execution fee and clears the job. The reservation charge covers booked attempts including unused ones.

Query `/stem/keeper-reservation` with any 36-byte packed sponsor OutPoint and `prove=true`. Decode the committed record with `stem_ledger::keeper::Reservation`. It identifies the signed trigger, the primary fee OutPoint and `command_id`. Use `"keeper":{"cancel":{"height":42,"fee_input":…,"command_id":"0x…"}}` or supply `timestamp_ms` in place of `height`. Sign a cancellation that consumes the identified fee Cell under its normal Lock before execution. A batch cancellation releases all its sponsor reservations. A normal spend leaves the queue entry for the Keeper to clear.

Query `/stem/keeper` with an eight-byte little-endian height. Query `/stem/keeper-time` with empty data for the timed index or the primary fee OutPoint for the command body. Verify each proof against the finalized Header. For completed attempts, query `/stem/execution`. Attempt indices precede submitted transaction indices. Existing development snapshots with pending Keeper entries use the earlier agenda and reservation encoding. Initialize a fresh development network for this revision.

Run the four-replica integration test and local storage measurement with the native RISC-V test dependencies available.

```sh
STEM_TEST_EVIDENCE_ROOT=/tmp/stem-keeper-evidence \
  cargo test --locked --release --test native_store \
  native_default_preserves_polytype_intent_lineage_and_keeper -- --ignored --nocapture
cargo test --locked --release --test native_store \
  measure_keeper_time_agenda -- --ignored --nocapture
```

The four-replica run also exports `lineage-references.json` with finalized
proofs for versions 0 and 4 of the same Lineage. Use the run directory
reported by the test to verify both proofs in the WASM SDK.

```sh
target/release/stem jolteon-trust --genesis "$RUN/genesis.json" > "$RUN/trust.json"
sdk/wasm/build.sh /tmp/stem-lineage-wasm
node sdk/wasm/tests/lineage.mjs /tmp/stem-lineage-wasm \
  "$RUN/lineage-references.json" "$RUN/trust.json"
```

For a 30-second mixed run with native Jolteon, select a fresh evidence
directory. The observer records all-replica visibility by polling and
verifies finalized execution proofs after the load stops.

```sh
STEM_TEST_EVIDENCE_ROOT=/tmp/stem-keeper-load \
  STEM_KEEPER_REPLICAS=4 STEM_KEEPER_RATE=6 STEM_KEEPER_PAYMENTS=24 \
  STEM_KEEPER_SECONDS=30 STEM_KEEPER_DRAIN=18 \
  cargo test --locked --release --test native_store \
  keeper_load::sustained_keeper_load -- --ignored --nocapture
```

Set `STEM_KEEPER_REPLICAS=16` for the larger matched run, or
`STEM_KEEPER_RATE=12` for the four-replica agenda-pressure run. Set
`STEM_KEEPER_HOT_HEIGHT=1 STEM_KEEPER_SECONDS=2 STEM_KEEPER_RATE=24
STEM_KEEPER_PAYMENTS=8 STEM_KEEPER_DRAIN=10` with four replicas for a
fixed-height contention run. RPC admission can precede discovery
that another proposal already booked the height. A reservation
takes effect once the carrier finalizes.

`stem wallet build --request request.json` encodes inputs, outputs, dependencies and optional ordered `messages` and `proofs`. `stem wallet sign` needs the native genesis, complete requested Lock Script, group index and `--context` file. Its `cells` array contains `{"out_point": ..., "cell": "0x..."}` records for all inputs, explicit dependencies and expanded Dep Group members. Its optional `headers` array contains canonical encoded Header Deps. Obtain and authenticate these records against the expected chain state before signing. `stem wallet id --network HASH` reads the JSON hex transaction from stdin. `stem ckb-wallet` uses the CKB-native encoding for L1 signing.

A separate payer can sign its Lock Group with `--sponsor-max-debit SHANNONS` after the participants fix the full transaction. The wallet requires plain Capacity inputs and change for that group. It checks that the payer's net debit fits the supplied cap and the actual total transaction fee. Each signer authorizes the same transaction and can add its seal independently.

`stem wallet find` discovers by complete Lock or Type Script hash in a local, read-only application database. Repeat the returned height and cursor for a consistent page sequence. Query selected OutPoints through ABCI for corresponding authenticated proofs.

With `prove=true`, `/stem/cell` returns a hex Strand `PatriciaPath` in its `proof` field. `/stem/execution` returns a hex Strand `ExecutionProof` with one position and count for both Merkle paths. Verify each proof against the corresponding root in the authenticated Stem Header.

Query `/stem/extinguished-fees` with `prove=true` and empty data to audit the cumulative fee tally at the current application height. The response contains `extinguished_shannons` as a hex u64 or `null` for an absent record, and a Strand Patricia proof. Authenticate the Header at the returned height through its signed successor, then verify against its SystemRoot using `Store::verify_extinguished_fees`. A verified absent record represents zero fees. The tally measures extinguished Capacity and can include fees from development allocations.

On a bridged network, `/stem/bridge-backing` with `prove=true` and empty data returns a Strand `record` and `proof`. It includes liquid reserve, circulating Capacity, pending withdrawal liability, accrued stake rewards and unallocated reserve as hex u64 values in shannons. Decode the record with `stem_ledger::protocol::BridgeBacking::decode` and the path with `stem_ledger::patricia::Proof::decode`. Verify the record using `Store::verify_bridge_backing` with the trusted network identity and Header at the returned height. CKB vault liveness and confirmations follow the guardian observation model.

Query `/stem/bridge-lane` with `prove=true` and the 32-byte lane ID as data. Decode its record with `stem_ledger::protocol::Lane::decode`, and check the lane ID, Header and record with `Store::verify_bridge_lane`. The returned counter alongside the Lane is the next request nonce. The Lane's `next_nonce` identifies the next withdrawal eligible for settlement.

CometBFT serves ABCI queries at `/stem/header`, `/stem/cell` and `/stem/execution`. A Live Cell query uses the packed OutPoint. An execution query uses a little-endian `u32` transaction index and a block height. Use `prove=true` for the Patricia path or the matching transaction and receipt Merkle paths. To authenticate header height H, verify the CometBFT commit at H+1 against the native AppHash. Supply signed handoff certificates when validator sets have changed.

## Exercise the native bridge

```sh
contracts_dir=contracts/target/riscv64imac-unknown-none-elf/release
python3 tools/native_bridge_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --comet target/stem-tools/cometbft \
  --vault "$contracts_dir/vault-type" --deposit-lock "$contracts_dir/deposit-lock" \
  --native-lock "$contracts_dir/stem-lock" --wrapped "$contracts_dir/bridge-udt" \
  --direct-checkpoint "$contracts_dir/direct-checkpoint" \
  --proof-verifier target/stem-tools/stem-proof --output target/stem-runs/native-bridge
```

Add `--xudt /path/to/xudt_rce --xudt-gate "$contracts_dir/xudt-gate"` to deploy a separate xUDT vault and execute both asset round trips. The runner uses isolated CKB and CometBFT nodes and disposable key files. At mint, guardians audit the actual confirmed consolidation and sign the exact native output bytes. To reconcile a payout, they authenticate the Stem execution receipt and the CKB transaction that matches its recipient, quantity and vault nonce.

To construct a release, query `/stem/bridge-withdrawal` with `prove=true` and 40 bytes of data containing the lane ID and little-endian `u64` nonce. Save the JSON response value and the native Header at the response height. Relay the signed successor to the vault's direct checkpoint. Use the checkpoint as a CKB Cell Dep, pay from a separate CKB input and place the command's result in the vault input's `WitnessArgs.input_type`.

```sh
stem bridge withdrawal-witness --genesis STEM_GENESIS --lane LANE --nonce NONCE \
  --header AUTHENTICATED_HEADER.json --proof WITHDRAWAL_QUERY_VALUE.json
```

The vault Type Script args are five 32-byte values in order. They encode the vault Type ID, direct checkpoint code hash, direct checkpoint Type ID, CKB genesis hash and Stem network ID. Use the intended checkpoint creation input and output index to commit its Type ID before writing the Stem genesis. The checkpoint bootstraps with the resulting Stem genesis AppHash. Keep the seed under deployment control through bootstrap, and pin the committed checkpoint's genesis committee, Stem genesis AppHash, chain ID and network before accepting deposits. Genesis checks the checkpoint code hash and Type ID across every vault and the Stake Registry. This makes the handoff available to vaults before retiring collateral can unlock. The configured Scripts must use the reviewed L1 executables and the published OutPoints must be confirmed on the intended CKB chain. Vault locks use the same executable with the vault Type Script hash followed by the 20-byte CKB multisig identifier. Consolidation, reserve movement and rotation require a separate input authorized by that multisig.

The runner also reconciles the recognized balance and liabilities against the amounts minted and paid on both lanes. The final report includes the ABCI backing record and path. Store and snapshot tests independently authenticate the path and reject altered amounts.

The runner saves the packed CKB payout transaction and its header with the dual Merkle proof. A wallet or relayer can validate a historical output offline with an independently trusted CKB block hash.

```sh
stem ckb-wallet verify-inclusion --transaction ckb-payout-packed.json \
  --proof ckb-payout-inclusion.json --block-hash "$confirmed_block_hash" \
  --tx-hash "$payout_transaction_hash" --output-index 0
```

The result authenticates the selected output at creation. It provides no authorization to consume the output or evidence about its later spend status.

### Read a CKB Cell from StemVM

Query `/stem/bridge-anchor` with `prove=true` and empty data to get the accepted CKB height and hash and the Stem adoption height. Verify the path with `Store::verify_bridge_anchor` and the finalized Header. Build a request with integer `anchor_height`, hex `anchor_hash` and a `references` array of one to four entries. Each entry supplies a CKB JSON `out_point` and the boolean `live_at_anchor`. Wait until the selected CKB anchor meets the configured confirmation depth. The guardian verifies inclusion for every output and checks each live request for unspent state on the same stable descendant tip.

```sh
stem bridge attest-reference --genesis STEM_GENESIS --l1-rpc CKB_RPC \
  --request reference.json --journal observer-journal --key-file guardian.key
stem bridge attach-references --genesis STEM_GENESIS --transaction draft.json \
  --observations reference-votes.json
```

Collect strictly more than two-thirds of guardian weight over the complete set. Each response contains the ordered `records`, `signer` and `signature`. Put these responses in one JSON array, attach them to the draft, then sign through the ordinary full Lock path. Use syscall 2130 and decode an individual Strand `L1Reference` at index `a3`. Applications can check the observation kind, CKB genesis and anchor, OutPoint, full Molecule CellOutput and Data. If the accepted anchor advances, have the guardians sign the complete set against the new anchor. A live record expires after the Stem Header Dep horizon measured from adoption of the anchor.

During a bridge lull, advance the anchor using the exact accepted predecessor and a strictly higher CKB height that already meets the network's confirmation depth. Use the same private guardian journal as for mint and settlement observations.

```sh
stem bridge attest-anchor --genesis STEM_GENESIS --l1-rpc CKB_RPC \
  --previous-height PREVIOUS_HEIGHT --previous-hash PREVIOUS_HASH --height CONFIRMED_HEIGHT \
  --journal bridge-journal --key-file guardian.key
stem bridge advance-anchor --observations anchor-votes.json
```

Collect strictly more than two-thirds of bridge guardian weight in `anchor-votes.json` and submit the returned signed control transaction. Genesis requires `bridge.anchor_limits` with integer operation, byte and signature budgets. Once the update finalizes, use the new anchor for observations and regenerate any pending mint or settlement certificates tied to its predecessor. This flow leaves the vault untouched.


Compile `stem-l1-reference` with the [RISC-V contract build flags](#build). Run `tools/jolteon_bridge_localnet.py` with the native CKB bridge options and `--reference-script /path/to/stem-l1-reference`. The fixture deposits the real sample Type Script into Stem, verifies both reference kinds, checks expiry, refreshes the anchor without moving any vault Cells and replaces the observed CKB fork.

### Exercise a paid CKB security domain

Build a CKB Data2 contract and run the real devnet fixture.

```sh
export CARGO_TARGET_DIR=/tmp/stem-security-domain-build
cargo build --locked --release --bin stem
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --bin security-domain
python3 tools/security_domain_localnet.py \
  --binary "$CARGO_TARGET_DIR/release/stem" --l1 /path/to/ckb \
  --contract "$CARGO_TARGET_DIR/riscv64imac-unknown-none-elf/release/security-domain" \
  --output /tmp/stem-security-domain-devnet
```

Use `stem security-domain domain --request domain.json` to derive the Lock,
Type ID, roster state and minimum funding. Fields include the deployed Data2
`code_hash`, first input `seed` (zero Since), `output_index`, `ckb_genesis`,
`network`, `operator`, `penalty`, `quorum_bps`, `challenge_blocks`, and
`members` entries with `public_key`, `beneficiary` and `bond`. Payees use
20-byte standard CKB Sighash arguments. Bond amounts use shannons and
must cover full payout capacity. Include the CKB genesis as the first
Header Dep of the registration transaction.

Derive a buyer escrow Lock with `stem security-domain request --request
request.json`. Include `code_hash`, `domain` (the domain Type Script hash),
`buyer`, `subject` (packed CKB OutPoint expressed as RPC JSON),
`refund_height`, and `receipt_capacity` in shannons. A receipt needs
at least 194 CKB. Fund the request for that capacity plus the service fee.
Budget at least 61 CKB for each selected Sighash payout. A signer runs
`stem security-domain sign --l1-rpc URL --domain domain-point.json --request
request-point.json --code-hash HASH --key-file KEY --journal PRIVATE_DIR`.
Each point file contains one RPC OutPoint. Keep the private signer journal
to preserve the one-result rule after restart.

For service, consume the domain and request as inputs 0 and 1. Put the
requested OutPoint first as a direct Code Cell Dep. Put operation byte 0,
the result hash, the roster-order bitmap and the ordered 64-byte signatures
in input 0's WitnessArgs `input_type`. Output 0 preserves the domain.
The payouts follow in signer order, then the receipt. Its Type args
concatenate the 32-byte domain hash and 36-byte request OutPoint. It stores
the 32-byte result. An ordinary fee input pays the CKB fee. The Rust
`stem_stake::security` module exposes canonical encodings, report digests
and verification. See the fixture for retirement, double-report evidence,
bond release and request refund transaction layouts.

To exercise the Stem consuming application, use the Jolteon bridge
fixture with `--security-domain PATH --reference-script PATH`. It
confirms the CKB receipt, builds a historical L1 Cell Reference after
that receipt has been spent, and requires the reference in a real
RISC-V Type Script. It also tests rejection of a missing or modified
record. [Run the Jolteon bridge fixture](#exercise-the-native-jolteon-path)

### Verify security-domain capital and income

Set the audit scope to the CKB genesis, Stem network, deployed Data2
`code_hash`, complete domain Type Script hash `domain`, and an independently
selected endpoint `through`. Use `{"cell":{"tx_hash":"0x…","index":"0x0"}}`
for a domain Cell or `{"closure":"0x…"}` for a closure transaction. Use
`--require-live` to require the selected Cell to still be live at the
observing CKB node. Choose a confirmation depth for the network.

```sh
stem security-domain audit --l1-rpc "$ckb_rpc" --scope scope.json \
  --confirmations "$depth" --proof-output history.json
stem security-domain verify-audit --scope scope.json --proof history.json \
  --anchors trusted-ckb-blocks.json
```

The online command can also print the verified report without exporting a
proof file. The offline verifier requires a JSON object mapping decimal CKB
heights to authenticated block hashes, including height zero and the block of
each transaction in the proof. Obtain the hashes from a trusted CKB full node
or another independently verified source on one canonical CKB chain. Use the
proof's heights to identify the required anchors. The receipt of a proof from
an indexer does not establish its Header hashes as trusted. Valid CKB
execution supplies input-side guarantees such as escrow funding and the
subject being live when the service ran.

Both commands report capital and actual service payouts through the requested
transition. Amounts are hexadecimal shannons. `capacity` includes the
domain's occupied capacity. `locked_bond`, `slashed_bond` and `returned_bond`
cover declared principal separately. `required_quorum_bond` uses the initial
roster. `can_attest` indicates whether the remaining eligible capital can
reach that quorum. Per-member `service_income` is the cumulative CKB credited
to that member's payout Lock, including before any slash or retirement. The
audit excludes buyer refunds, returned principal and the operator's reserve
from service income.

The offline report describes the selected historical endpoint. A later spend
or a CKB reorganization may change the current view. The Rust API is
`stem::security_audit::verify`. Proof size and verification cost grow with the
number of domain transactions. Online collection defaults to 256 transactions
and accepts up to 2,048 via `--max-transactions`. The offline CLI accepts proof
files up to 64 MiB and 2,048 transactions.

Run the standalone fixture above with `--audit-reorg` to reorg a confirmed
closure on the disposable devnet and verify that the old proof fails against
the replacement block hash.

### Exercise Intercell Messaging

Build the native CKB `intercell-mailbox` binary and use the normal Jolteon bridge fixture with its `--intercell-mailbox` option. The deployer fixes the CKB genesis, the active Stem network, the mailbox owner Lock hash, and the same checkpoint used by genesis. The mailbox Type Script uses Data2 addressing and its arguments concatenate Type ID, CKB genesis, Stem network, owner Lock hash, Checkpoint code hash and Checkpoint Type ID, in that order. Initial data consists of a zero little-endian nonce and a zero 32-byte replay root. The deployment supplies the CKB genesis as its first Header Dep.

```sh
export CARGO_TARGET_DIR=/tmp/stem-intercell-build
cargo build --locked --release --bin stem
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --bin intercell-mailbox
python3 tools/jolteon_bridge_localnet.py \
  --binary "$CARGO_TARGET_DIR/release/stem" --l1 /path/to/ckb \
  --vault "$programs_dir/vault-type" --deposit-lock "$programs_dir/deposit-lock" \
  --native-lock "$programs_dir/stem-lock" --wrapped "$programs_dir/bridge-udt" \
  --direct-checkpoint "$programs_dir/jolteon-checkpoint" \
  --intercell-mailbox "$CARGO_TARGET_DIR/riscv64imac-unknown-none-elf/release/intercell-mailbox" \
  --output /tmp/stem-intercell-devnet
```

Add `--exercise-publisher` to drive the fixture through the sparse publisher. The fixture first delivers against an exact checkpoint, then advances it and delivers an older Outbox proof with a history path. The generic bidirectional fixture receives a message with three CKB references under the usual 20-million-cycle transaction cap. It exercises the 4,096-byte payload limit on both chains.

Use `stem intercell encode --request message.json`. The request fields are `source`, `destination`, `sender`, `receiver`, `nonce` and hex `payload`. The result contains the hex message `record` and stable `id`. Include the record as a Group Action with kind 65280 for sending or 65281 for receiving. For a receive, put the mailbox output with `live_at_anchor: false` and any application references in one `attest-reference` request at the currently accepted anchor. Pass the guardians' votes for that set to `attach-references`. Exactly one historical reference must match the complete outgoing record. A historical reference can prove an outgoing mailbox record after its output is spent. Sign the assigned consuming Lock group after attaching the certificate.

For CKB delivery, query `/stem/intercell/outbox` with the message ID as hex `data` and `prove=true`. Use a live Jolteon Checkpoint Cell at or after the proof height. Save the Outbox proof while its height is the Stem tip. Pass the query's `record` and `proof` and the live Mailbox's `expected_root` to `stem intercell delivery-witness --request delivery.json --header header.json`. Supply exactly one of `consumed` (an array of consumed IDs) and `replay_path` (a hex-encoded sparse non-inclusion proof for the message ID at that root). The Header file holds the execution Header from the live Checkpoint Cell as a JSON hex string. The command verifies the Outbox and replay paths and derives the successor root. Put the returned `input_type` in the mailbox input's Molecule WitnessArgs and the `successor_root` in the successor data. Keep the outbound nonce and use empty outgoing bytes. The CKB transaction references that live checkpoint and the owner signs the transaction. A relay can maintain `stem_stake::intercell::Inbox` and use `Inbox::path` to construct a proof without passing its full index to the witness builder.

If the live checkpoint has advanced beyond the Outbox proof, generate a history proof from an archive. The historical proof uses the checkpoint height as its MMR leaf count. If the checkpoint advances again before the CKB transaction commits, rebuild the history proof for the new checkpoint and sign the updated CKB transaction. The saved Outbox proof can be reused.

```sh
stem checkpoint history --archive-db "$stem_archive_db" --index "$history_index" \
  --network "$stem_network" --height "$checkpoint_height" --header "$outbox_height" > history.json
stem intercell delivery-witness --request delivery.json --header checkpoint-header.json \
  --history history.json
```

Build the Mailbox, Stem CLI and any CKB settlement Locks that consume its witness from the same revision. The delivery witness has three segment lengths for the record, Outbox path and optional history proof. The witness ends with the replay path.

If the relay loses its local inbox index, use the live CKB mailbox OutPoint and the pinned complete Mailbox Type Script hash to rebuild it with a CKB full node that serves historical transactions and witnesses.

```sh
stem intercell recover-inbox --l1-rpc http://127.0.0.1:8114/ \
  --tx-hash "$mailbox_head_tx" --output-index "$mailbox_head_index" \
  --mailbox "$mailbox_type_hash" > inbox.json
jq -s '.[0] + (.[1] | {consumed, expected_root})' outbox-proof.json inbox.json > delivery.json
stem intercell delivery-witness --request delivery.json --header header.json
```

Recovery identifies the actual mailbox input and output at each step, checks the canonical block hashes and replay root transitions, and checks the supplied head is still live before returning. For each delivery it also asks the CKB node for the block Header and the transaction and witness proof, then verifies both paths locally. The relay relies on this CKB node for its canonical chain view. A stale or orphaned head requires a fresh live OutPoint. The default budget is 2,048 transaction reads, including unrelated inputs read to identify the mailbox predecessor. Use `--max-transactions` for a deeper history, up to 65,536 reads. Relays with longer histories should maintain a derived local inbox index. The mailbox contract still verifies the resulting witness at delivery time.

A Stem recipient can verify inclusion or absence at the finalized tip via `/stem/intercell/inbox` and `Store::verify_intercell`. The fixture captures failures and successful inclusion transactions in its output directory.

### Exercise Stem native UDT settlement

Build the following RISC-V contracts with the flags from the [Intercell exercise](#exercise-intercell-messaging). Use its bridge runner options and add the three UDT binaries. Include `--full-udt-return` with a fresh output directory to exercise release of the entire exported amount.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --bin stem-native-udt \
  --bin stem-asset-escrow --bin ckb-stem-udt
```

Set `--export-udt` to `stem-native-udt`, `--asset-escrow` to `stem-asset-escrow`, and `--ckb-stem-udt` to `ckb-stem-udt`, in addition to `--intercell-mailbox`.

The Stem UDT Type Script uses the issuer Lock hash followed by the asset series as args. The Stem escrow Lock args contain seven 32-byte values in order. These are the UDT Type Script hash, CKB genesis, Mailbox code hash, Mailbox Type ID, CKB UDT code hash, Checkpoint code hash and Checkpoint Type ID. Derive the Mailbox Type ID from its designated CKB creation input and output index. Then compute the complete Stem escrow Lock hash.

On CKB, use the `ckb-stem-udt` code as the Mailbox's Lock with Data2 addressing. Its args are the Mailbox Type ID, the Stem escrow Lock hash, the Stem asset Type Script hash and the Mailbox code hash. Set the Mailbox policy owner to that Lock's complete Script hash. The CKB wrapped UDT Type Script uses the same program with one argument, the complete Mailbox Type Script hash. Use `stem intercell encode` with the 80-byte asset payload and the ordinary checkpoint and historical-reference workflow. The Rust `stem_bridge_wire::asset` module provides the payload encoding and derives the Stem export nonce from the custody OutPoint.

Register and publish the actual route identities before sending funds. The fixture publishes all executable code Cells, creates the mailbox and custody bond, and saves both chains' signed transactions and proof records.

### Exercise CKB Spore settlement

Use the published Spore V2 code Cell from the deployment transaction
`96b198fb5ddbd1eed57ed667068f1f1e55d07907b4c0dbd38675a69ea1b69824` at output zero.
Its raw ELF must have CKB data hash `4a4dce1df3dffff7f8b2cd7dff7303df3b6150c9788cb75dcf6747247132b9f5`.
The fixture publishes these same bytes on the disposable CKB chain under Data1 addressing.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --bin stem-spore --bin ckb-spore-escrow
```

Use the usual [Intercell bridge command](#exercise-intercell-messaging) with `--stem-spore`, `--ckb-spore-escrow` and `--spore-v2` set to the three binaries. Choose a fresh output directory for each run. The runner exercises issuance with the original Type Script, CKB escrow, Stem import and ownership transfer, CKB release and redeposit.

Add `--spore-cluster-v2` with the published Cluster ELF to exercise the same path with Cluster membership. Use output zero of deployment transaction `e464b7fb9311c5e2820e61c99afc615d6b98bdefbe318c34868c010cbd0dc938`. The runner checks its CKB data hash `7366a61534fa7c7e6225ecc0d828ea3b5366adec2b58206f2ee84995fe030075`, mints the Cluster and member with the original contracts, and transfers the Cluster on CKB while the member circulates on Stem. [Spore deployment records](https://github.com/sporeprotocol/spore-contract/blob/master/docs/VERSIONS.md)

For manual construction, first obtain the original complete Spore Type Script hash and fix the Mailbox creation input and output index. The Stem endpoint Lock uses `stem-spore` with Data addressing. Its args are the original Type Script hash, CKB genesis, Mailbox code hash, Mailbox Type ID, CKB escrow code hash, Checkpoint code hash and Checkpoint Type ID. Its plain bond Cell must keep the same capacity on each transition. The Stem mapping uses the same program as a Type Script with the full endpoint Lock hash as its only argument.

The CKB escrow Lock uses `ckb-spore-escrow` with Data2 addressing. Its args are the Mailbox Type ID, Stem endpoint Lock hash, original Type Script hash and Mailbox code hash. Set the Mailbox owner to this full Lock hash. Both the CKB deposit and release must transfer the original Spore with a valid CoBuild action in the extra witness. Use the existing Intercell proof commands and keep the original Cell capacity unchanged across custody.

The first 64 bytes of the message payload hold the original Type Script hash and destination Lock hash. Append the original canonical Molecule data, including its Cluster ID when present. Rust clients can inspect the bytes with `stem_bridge_wire::spore::SporeData::decode`. The source CKB capacity must cover the bytes and the escrow Lock at deposit. For the Stem return, hash `Stem Spore return nonce\0` followed by the canonical 36-byte bond OutPoint with CKB Blake2b. Interpret the first eight digest bytes as a little-endian integer. Retain the consumed-message IDs needed to reconstruct the mailbox replay path. Direct transfers to the escrow Lock require the matching Mailbox update to be recognized as deposits.

### Exercise Stem native object settlement

Build the contracts and use the [Intercell runner](#exercise-intercell-messaging) with
`--stem-native-object`, `--object-policy`, and `--ckb-stem-object` set to the three contract executables.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --bin stem-native-object --bin stem-object-policy --bin ckb-stem-object
```

The first four 32-byte values in the Stem identity arguments are the object ID, Stem approval Lock hash, CKB approval Lock hash and Stem policy code hash. Derive the object ID with `stem_bridge_wire::object::creation_id` and use Data addressing for the identity and policy scripts. The policy arguments concatenate the complete identity Type Script hash and the Stem approval Lock hash. The object data uses the plain Molecule `SporeData` profile with an empty Cluster field.

The Stem escrow arguments use the original complete identity Type hash, CKB genesis, Mailbox code hash, Mailbox Type ID, CKB object code hash, Checkpoint code hash, Checkpoint Type ID and CKB approval Lock hash. Calculate the escrow Lock before creating the CKB Mailbox. The CKB Mailbox uses the `ckb-stem-object` Data2 Lock with arguments Mailbox Type ID, Stem escrow Lock hash, object Type hash, Mailbox code hash and CKB approval Lock hash. The CKB representation uses the same program as its Type Script. Its arguments are the full Mailbox Type hash, the full Mailbox Lock hash and the CKB approval Lock hash.

Approval and ownership have their own inputs and signatures. The fixture uses a single approval key under two network-specific Locks and distinct owner keys. It archives successful and rejected transactions through creation, export, CKB transfer, verified burn and restoration, including attempts to remove the independent policy or pay the remote escrow Lock.

### Measure native storage and sustained payments

Start a native node with `stem run --storage-metrics` to enable the
local diagnostic query `/stem/storage-metrics`. Request the current tip
with empty data and `prove=false`. The counters reset on process restart.

The query counts successful synchronous RocksDB writes and partitions the
serialized WriteBatch bytes by key family. It reports proposal journal,
other consensus records, execution, Headers and history, Live Cells, system
records, both Patricia node namespaces and batch framing. Bootstrap, journal
writes, finality commits and snapshot restoration have separate counters.
Sync buckets use powers of two microseconds. Bucket zero covers zero and
one microsecond. RocksDB reports WAL bytes and syncs. The bundled
CKB RocksDB build disables IOStatsContext, so flush and compaction
byte tickers can stay at zero during table writes. WAL, SSTs and
file-system activity are shared across the key families in the atomic
store. Use the table creation events from a completed, fresh fixture
directory to measure actual SST output:

```sh
python3 tools/rocksdb_sst_report.py /tmp/stem-storage-measurements/stem-storage-RUN
```

The output includes current and retained rotated logs, classifies
flush, compaction and recovery jobs, and covers database startup
through the last available log event.

Run the sustained fixture with the local filesystem used for the test:

```sh
STEM_TEST_EVIDENCE_ROOT=/tmp/stem-storage-measurements \
STEM_STORAGE_RATE=400 STEM_STORAGE_SECONDS=45 STEM_STORAGE_DRAIN=8 \
cargo test --locked --release --test native_store \
  sustained_payment_storage -- --ignored --nocapture
```

The fixture uses real signed payments, four Jolteon replicas, three 64-block
retention nodes and one archive by default. `STEM_STORAGE_METRICS=0` runs
the control without RocksDB statistics or per-batch instrumentation.
The fixture preserves signed workload bytes, per-payment offer and admission
times, per-replica status and process statistics, sampled common finality,
the certified Header and a separate history-index process measurement.
Set `STEM_STORAGE_REPLICAS`, `STEM_STORAGE_VM_WORKERS`,
`STEM_STORAGE_BLOCK_CYCLES`, `STEM_STORAGE_CADENCE` or
`STEM_STORAGE_RETENTION` to make an explicit comparison.
Use `STEM_TEST_REPLICA_BIN` to compare node releases with the same
fixture executable and a fresh evidence directory for each run.
To compare submission connections, set `STEM_STORAGE_REUSE_SUBMISSIONS=0`
for fresh sockets, then set it to `1` against the same node binary.
The report counts per-payment socket reuse and measures socket readiness
and reply latency alongside replica admission queue and service time.
For a comparison of VM cache revisions, keep the signed workload,
transaction limits, sender count and worker count fixed. Run both a
rate every client can sustain and a higher rate that exposes overload
behavior. Compare completed writes and common visibility separately.

For a state-read comparison, run the same signed payments and then
repeat with `STEM_LINEAGE_WORKLOAD=distributed_amm` and the
`sustained_lineage_load` test filter. The latter verifies finalized
receipts, state roots and AMM balances by serial replay.
Set `STEM_STORAGE_PAYMENT_DATA_BYTES=2048` on both sides of a
payment comparison to create signed outputs with 2 KiB of fixed
Cell Data. The default is an empty payload. Match this setting
when comparing memory or database behavior.
The RISC-V parent-cache controls can be run with
`cargo test --locked --release -p stem-vm --test native block_context`
and the same command with `payment_verification_reuses_cycles`.
Use `STEM_STORAGE_SENDERS=64 STEM_STORAGE_RATE=2000 STEM_STORAGE_SECONDS=20`
for the short high-rate comparison.

For the assembly trace and instruction-dispatch comparisons, build both node revisions with
`cargo build --locked --release --bin stem` and pin one
`native_store` test executable for both. Set
`STEM_TEST_REPLICA_BIN` to each node binary in turn. Use a fresh
evidence directory per run, four replicas, 64 senders, fresh sockets,
two VM workers, a 250 ms proposal interval, a two-billion-cycle block
budget and a 12-second drain. The matched runs use 1,000 TPS for 35
seconds and 2,000 TPS for 24 seconds. The benchmark archive contains
the exact commands and the isolated VM probes.

For the fixed Patricia editor probe, run:

```sh
STEM_PATRICIA_REPORT=/tmp/stem-patricia-editor.json \
cargo test --locked --release --lib measure_patricia_node_storage \
  -- --ignored --nocapture
```

For the fixed-buffer read comparison, build the test executable at
`87b5730` and at `fc49e54` in separate worktrees or retain a copy of
each executable. Run the probe in baseline, candidate, candidate,
baseline order. Use a separate `STEM_PATRICIA_REPORT` for each run.
Compare the compact samples by edit duration, canonical reconstruction
time, node bytes and resulting root. The archived commands also run
the same signed payment workload on four Jolteon replicas per version.

The RocksDB point-read experiment compares default options, reused
options and pinned values on the same native Patricia edit. It repeats
the reads after a synchronous flush to a warmed SST.

```sh
STEM_PATRICIA_READ_REPORT=/tmp/stem-patricia-point-reads.json \
cargo test --locked --release --lib measure_patricia_point_reads \
  -- --ignored --nocapture
```

For the fixed-tip admission comparison, run the signed AMM probe
with direct serial checks, blocking single-request dispatch and
parallel groups. All three paths compare receipts.

```sh
STEM_ADMISSION_REPORT=/tmp/stem-admission-dispatch.json \
cargo test --locked --release --test native_store \
  measure_lineage_admission_groups -- --ignored --nocapture
```

To measure Stake Registry loading during admission, use a stopped
Stake-backed replica or restore the archived fixture snapshot to a
new observer. Extract the genesis, guardian observations, signed
transactions, snapshot and finality certificate from the benchmark
archive. The genesis and its referenced trust and observation files
must be in the same directory. Point the probe at the observer's
`db` directory after checking the snapshot Header ID.

```sh
STEM_FUNDED_ADMISSION_GENESIS=/tmp/stem-admission/stem-genesis.json \
STEM_FUNDED_ADMISSION_DB=/tmp/stem-admission/observer/db \
STEM_FUNDED_ADMISSION_WORKLOAD=/tmp/stem-admission/signed-payments.json \
STEM_FUNDED_ADMISSION_REPORT=/tmp/stem-admission/timings.json \
cargo test --locked --release --bin stem measure_funded_admission \
  -- --ignored --nocapture
```

For the baseline comparison, add only the probe from
`src/native_spec.rs` at `cd488ea` to a separate worktree at
`754b1ef`. The archived commands preserve the probe order and
fingerprints.

Use the same three fixture paths for the block preparation probe.
It compares empty blocks, single-payment blocks and children of a
candidate with a fee update.

```sh
STEM_FUNDED_PREPARATION_REPORT=/tmp/stem-admission/preparations.json \
cargo test --locked --release --bin stem measure_funded_stake_preparation \
  -- --ignored --nocapture
```

For its baseline, use the probe source in `src/native_spec.rs`
at `183c7dd` against the runtime from `88fac89`.

Build the release library test executable from each revision to
count outgoing TCP calls for the native frame comparison. Copy
both executables before rebuilding the same target directory.

```sh
strace -f -c -e trace=sendto,sendmsg,writev /path/to/stem-library-tests \
  jolteon::transport::tests::submission_sessions_are_sequential_and_bounded --exact
```

For the network comparison, pair the Rust load test executable
that links each client revision with a fixed node binary using
`STEM_TEST_REPLICA_BIN`. Match the connection reuse setting,
signed workload and database instrumentation across revisions.

### Measure sustained Lineage commands

Run the native load fixture with real Lineage registration, signed fee
Cells and RISC-V Executors.

```sh
STEM_TEST_EVIDENCE_ROOT=/tmp/stem-lineage-measurements \
STEM_LINEAGE_WORKLOAD=distributed_amm STEM_STORAGE_REPLICAS=4 \
STEM_STORAGE_RATE=240 STEM_STORAGE_SECONDS=30 STEM_STORAGE_DRAIN=12 \
cargo test --locked --release --test native_store \
  sustained_lineage_load -- --ignored --nocapture
```

`distributed_amm` assigns each of 32 traders its own pool.
`hotspot_amm` uses the same development state, scripts and signers
and sends every trade through the first pool. Each trade atomically
changes a pool and a trader reserve and executes both asset Type
Groups. `independent` rotates across 64 counters, `atomic` increments
eight counters per command, and `mixed` adds a shared counter to every
third command. Set `STEM_STORAGE_VM_WORKERS=1` for the serial worker
control or `STEM_STORAGE_REPLICAS=16` for the larger local network.

The fixture samples common finality during offer and drain. After
capturing resource counters, it stops the replicas and serially
replays the archive in a separate store. The audit checks every
receipt and Header, the final versions and both proof paths for
each Head, and the two asset supplies for AMM runs.

Set `STEM_TEST_REPLICA_BIN` to a fixed Stem executable for a before/after
network comparison. The same load driver launches that executable and
uses it for the history index. Status samples include current and high
views, observed timeout certificates, and the most recent view whose
proposal failed the local timestamp window.

The load driver submits at the configured rate through one bounded
queue. Set `STEM_STORAGE_SENDERS` to set both the number of workers
and the queue capacity. Each planned submission reports when it was
queued and attempted, when TCP connected, when the full frame was
written, and when a complete reply was received. A successful
admission requires a matching transaction digest. The report counts
unattempted submissions, transport write failures, full frame writes
and positive admissions separately. The `submission_us` fields
summarize each measured stage. The connect stage includes building
the request frame. A write timestamp marks the local socket write.

The `admission` status counters report client requests, received
transaction relays, Store checks, accumulated check microseconds and
local relay retry attempts. The `client_queue` histogram starts when
the server decodes a complete submission and ends when the replica
starts handling it. The `client_service` histogram ends when the
reply is encoded. Each reports cumulative microseconds and counts
with inclusive upper bounds of 1, 10, 100, 500 and 2,000 ms and
a final overflow bucket.
Use start and end deltas in load tests. The fixed-state comparison
isolates verification of signed AMM commands over a single tip.

The `execution` status field uses the same buckets for Leader
proposal construction, uncached candidate verification and certified
commit. Proposal timing covers selecting and constructing the
execution batch. Candidate verification measures decoding and executing
an uncached proposal. Commit measures the synchronous Store call for
each finalized execution block. These are elapsed times on the local
replica and can include time waiting for CPU.

```sh
STEM_ADMISSION_REPORT=/tmp/stem-admission.json \
cargo test --locked --release --test native_store \
  measure_lineage_admission_groups -- --ignored --nocapture
```

To measure only proposal selection against an exhausted read budget,
run the fixed-queue test. For checkout comparisons use the same
directory of prebuilt RISC-V contracts in both runs.

```sh
STEM_TEST_CONTRACT_DIR="$contracts_dir" STEM_SELECTION_REPORT=/tmp/stem-selection.json \
cargo test --locked --release --test native_store \
  measure_exhausted_lineage_selection -- --ignored --nocapture
```

The report includes all selection times, the signed input digest,
and the Header produced by the selected transactions.

### Exercise the browser SDK

Build the ES module and TypeScript declarations with the [SDK build
instructions](sdk/wasm/README.md). Run the generic [Intercell devnet
fixture](#exercise-intercell-messaging) with `--wasm-fixture` and keep its
`wasm-fixture.json`. Run the [CKB-backed stake fixture](#exercise-ckb-delegation)
with `--publish-checkpoint --delegation --retire --slash --reward-accounting`
to capture two real handoffs.

```sh
sdk_dir=/tmp/stem-wasm-package
cp sdk/wasm/tests/{smoke.mjs,node.mjs,browser.html,types.ts} "$sdk_dir/"
cp "$intercell_run/wasm-fixture.json" "$sdk_dir/fixture.json"
python3 sdk/wasm/tests/handoff.py --binary "$stem_binary" \
  --run "$stake_run" --output "$sdk_dir/handoff.json"
node "$sdk_dir/node.mjs"
tsc --strict --noEmit --target ES2022 --lib ES2022,DOM,ESNext.Disposable \
  --module ESNext --moduleResolution bundler "$sdk_dir/types.ts"
python3 -m http.server 33800 --bind 127.0.0.1 --directory "$sdk_dir"
```

Open [the local fixture](http://127.0.0.1:33800/browser.html) to run the
same proofs in a browser. The result element records `data-result="pass"`
on success. The fixture compares browser-built transaction and seal bytes
to the transaction actually finalized by the nodes.

### Verify per-vault revenue

Query `/stem/vault-revenue` with `prove=true` and empty data at the current
tip. Decode the query value to `accounting.json` and authenticate the native
execution Header at the returned height with `stem jolteon-proof`. Save that
Header as a JSON hex string in `header.json`.

```sh
stem bridge verify-vault-revenue --network "$stem_network" \
  --header header.json --accounting accounting.json
```

The CLI checks the Strand revenue record and cumulative fee tally against
the same SystemRoot, then recomputes the amount for each lane. Amounts in the
returned account report are hexadecimal shannons. Allocation uses each
lane's previously authenticated CKB reserve weight. See `/stem/fee-funds`
for the actual withdrawal lane and amounts already paid. The Rust SDK exposes
`VaultRevenue::decode` and `Store::verify_vault_revenue`.

### Redeem fee revenue on CKB

An optional `bridge.fee_funds` genesis array contains four objects with `basis_points` and a CKB JSON `recipient` Lock. The fixed order is validators and delegators, DevFund, Ecosystem Fund and PoW Security Fund. Use distinct recipients matching the allowlisted Lock templates. Shares are positive and total 10,000. Reserve capacity must be available in the native CKB lane. Each payout must meet the recipient's CKB occupied Capacity.

Request `/stem/fee-funds` with `prove=true` at the current finalized height. Authenticate its `policy_record`, `payout_record` and cumulative fee tally against the finalized Header using `Store::verify_fee_fund_policy`, `Store::verify_fee_payouts` and `Store::verify_extinguished_fees`. The two Strand fund records can be decoded with `stem_ledger::protocol::{FeeFundPolicy, FeePayouts}`. Supply the chosen account's `queued_shannons` as a decimal integer. The result is a JSON hex transaction suitable for the ordinary submission endpoint.

For a Stake Registry network, fund 0 uses individual reward claims. The first fund entry supplies its allocation rate. Its available amount is an aggregate ceiling that includes open-epoch earnings and rounding residue. The other fund entries also supply their fixed payout Locks. [Verify closed-epoch stake rewards](#verify-closed-epoch-stake-rewards)

```sh
stem bridge claim-fees --fund 3 --amount 8000000000 --claimed-before 0
```

An accepted claim creates a pending withdrawal in the native CKB lane. Use the next lane nonce with the same checkpoint relay and `stem bridge withdrawal-witness` flow as a user withdrawal. Anyone can relay the release to the fixed CKB recipient. The separate CKB fee input pays the relay costs. Guardians check that the L1 transaction paid the prescribed recipient and amount. An accepted `bridge settle` confirms the payment in Stem and removes its pending marker.

For a four-replica devnet run, use the same binary and contract options as the [Jolteon bridge exercise](#exercise-the-native-jolteon-path), replace both xUDT flags with `--fee-funds`, and choose a new output directory. The fixture pays a deliberately high 800 CKB transaction fee from a confirmed bridged Cell, allocates it at 65/15/10/10 and redeems all four accounts on CKB. The output directory contains the success and rejection evidence. The fixture's fee and allocation rates are experimental values.

### Calibrate native resource prices

For genesis pricing, specify `encoded_byte_microshannons`, `cycle_microshannons`, `cell_read_microshannons`, `cell_read_byte_microshannons`, `cell_write_byte_microshannons` and `cell_delete_microshannons` in `fees`. All six are integer rates with one million units per shannon. The precision enters the genesis commitment. Recreate disposable networks for this fee schedule.

```sh
python3 tools/jolteon_localnet.py --binary target/release/stem \
  --lock "$programs_dir/stem-lock" --fee-calibration --output /tmp/stem-fee-calibration
```

The runner sends 32 signed one-input, two-output payments at 140 shannons, compares a 139-shannon payment using the same input against the admission limit, and records the committed fee proof. The rates in this fixture are for the measured standard Lock and transaction shape.

## Exercise the CKB Stake Registry

```sh
python3 tools/native_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --comet target/stem-tools/cometbft \
  --stake-registry "$contracts_dir/stake-registry" --direct-checkpoint "$contracts_dir/direct-checkpoint" \
  --proof-verifier target/stem-tools/stem-proof --output target/stem-runs/native-stake
```

The runner registers and increases CKB bonds, rotates snapshot guardians,
verifies both actual CometBFT handoffs at H+2, retires an excluded key with
the direct checkpoint and exercises delayed withdrawal. Add `--slash`
to freeze and slash a fixture bond on CKB. That run verifies one precommit
recorded by CometBFT and a local conflicting nil precommit for the same
slot. The latter stays in the CKB test evidence. Use the emitted
directory for proof bundles and exact CKB inclusion heights. Keep the
absolute output path short enough for the local ABCI Unix sockets.

Use `stem stake retirement-proof` with the retired public keys from confirmed CKB Registry history. Compare its returned root with the current Registry Cell. A registration supplies the absence proof after the 64-byte registration signature. A delayed withdrawal supplies the proof as its operation payload and installs the returned next root. The L1 Stake Cell and Registry are consumed atomically. The old public key stays retired after the active slot is released.

Query `/stem/stake-retirement` at the current height with `prove=true`
and the 32-byte validator public key as data. Save its decoded response
value and the native Header at its height. Advance the Registry's
direct checkpoint to this Header using the signed CometBFT successor.
Build the canonical Patricia path with the following command.

```sh
stem stake retirement-witness --genesis STEM_GENESIS --validator VALIDATOR_PUBLIC_KEY \
  --header AUTHENTICATED_HEADER.json --marker RETIREMENT_QUERY_VALUE.json
```

Consume the current Registry and Stake Cells together. The Registry
input's CKB `WitnessArgs.input_type` holds operation byte `0x04`, the
32-byte validator public key and the returned path. Reference the live
direct checkpoint as a Cell Dep and put the proved effective height in
the updated Stake Cell. The validator's owner still authorizes withdrawal
using the configured relative block delay.

Stake Registry Type Script args encode its 32-byte CKB Type ID, 32-byte
Stem network, 32-byte CKB genesis, three little-endian u64 values for
minimum bond, blocks per epoch and unbonding delay, the direct checkpoint
code hash and Type ID, and the CometBFT chain ID. Reserve the checkpoint
seed OutPoint and output index before deriving the Registry identity
and roll the exact resulting Stem genesis into the checkpoint bootstrap.
Control the seed until the checkpoint has been initialized with that
genesis. Snapshot guardians still authorize activation and attest CKB
observations. For rotation, provide the previous confirmed Registry
OutPoint and outgoing committee to `stem stake observe`, then submit
observations from both sets.

To benchmark during committee changes, add the following flags to the native stake runner. The runner makes a real CKB deposit, mints native Capacity, splits it into independent payment inputs and signs each payment before the timed window. The optional workload uses two same-host replicas with a three-second commit wait. The window includes both validator updates.

```sh
--loadgen target/stem-tools/stem-loadgen \
  --vault "$contracts_dir/vault-type" --deposit-lock "$contracts_dir/deposit-lock" \
  --native-lock "$contracts_dir/stem-lock" --wrapped "$contracts_dir/bridge-udt"
```

### Exercise CKB delegation

Deploy the current default `stake-registry` contract and pin its new Type Script in a fresh devnet genesis. Use the Jolteon stake runner with `--publish-checkpoint --retire --delegation`. It funds positions from two distinct CKB Locks, creates a separate top-up position, verifies the committee weights and settles after authenticated retirement. Add `--slash --active-slash` in a separate run to freeze the validator while it is serving and settle the delegated penalties after its exclusion. Use a new output directory and CKB port for each run.

Add `--delegation-power-cap` to test a power cap of 800 against the combined raw epoch-one weight of 895. The fixture checks the effective weights of 442 and 357 after normalization.

```sh
python3 tools/jolteon_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --stake-registry "$native_dir/stake-registry" \
  --direct-checkpoint "$native_dir/jolteon-checkpoint" \
  --publish-checkpoint --retire --delegation --output /tmp/stem-delegation
```

`stem stake delegation-script` takes the Registry Type Script JSON, the validator key, the beneficiary CKB Lock hash, the JSON first `CellInput` and the output index. Put its returned `type` and `lock` Scripts on the funded output and keep the 128-byte `identity` for Registry witnesses. The identity can be recovered from a confirmed creation transaction's Registry input witness. The funded Cell's five little-endian `u64` values are active principal, pending principal, Registry sequence, confirmed exclusion height and confirmed activation height. When activation height is zero, the sequence records creation. After activation, it is zero until the holder requests a reduction, then carries that request's sequence. At creation set the active principal and creation sequence, with zero for the other fields. The funded Capacity is active plus pending principal plus 146 CKB of occupied Capacity. Create a separate position for each increment. The beneficiary authorizes funding and reduction requests. A relayer can submit proof confirmation and settlement with a separate fee input.

The Registry input witness contains one operation byte, the 128-byte identity preimage, a little-endian u16 length and the active principal proof of that length, followed by any operation proof. Use `0x09` for creation, `0x10` for activation, `0x0a` for unbonded withdrawal on full validator retirement and `0x0b` for its penalty settlement. The validator's active and unbonding totals in Registry data must reflect the exact principal deltas. When active principal changes, build a path from the predecessor Registry root with `stem stake delegation-proof`. Supply the confirmed active positions for this validator and the target identity with its successor principal. The CLI checks the reconstructed root and returns the canonical proof and new root. Encode a zero proof length for operations that only change pending principal or status. For an unbonded withdrawal on full validator retirement add the live retired validator Stake Cell and its creation block as Cell and Header Deps, and set the delegation input's absolute block `since` to the retirement block height plus the configured delay. The payout must go to the beneficiary Lock as a plain CKB Cell. A small partial payout may share the beneficiary's standard change output to meet CKB occupied Capacity. A penalty settlement pays at least the forfeited principal as CKB transaction fees and returns the occupied Capacity to that Lock if the position closes.

For an in-term exercise, run the following command with the same binary and contract paths, using a fresh output directory. Include `--reduction-slash` for the historical double-vote case. Use `--reduction-reorg` on a separate disposable devnet to replace the observed CKB block and verify rejection of both the orphaned and substituted anchors.

```sh
python3 tools/jolteon_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --stake-registry "$native_dir/stake-registry" \
  --direct-checkpoint "$native_dir/jolteon-checkpoint" \
  --publish-checkpoint --delegation --partial-reduction --output /tmp/stem-delegation-reduction
```

Use `0x0c` to move an owner-authorized amount from active delegation to unbonding. Put the resulting Registry sequence and zero cutoff in the successor. Use `0x0d` to confirm the cutoff with an authenticated checkpoint. Activation (`0x10`) and cutoff confirmation use the same proof layout. Append the applied sequence, prior applied sequence, effective Stem height and CKB observation block height as little-endian `u64` values, then the 32-byte CKB observation block hash and Patricia path. Query `/stem/stake-registry-application` with the Registry ID and little-endian applied sequence to obtain these fields. Supply CKB Header Deps for the creation or reduction Cell and its snapshot observation block. Use `0x0e` to settle after the successor's relative CKB `since` delay. Use `0x0f` to settle a pending penalty, appending a historical double-vote witness and the pinned checkpoint dependency if the Registry's existing fault lacks two vote heights inside the position's liability interval.

### Verify closed-epoch stake rewards

Query `/stem/stake-reward-epoch` with `prove=true` and the eight-byte little-endian epoch number. Use the returned height to fetch the signed Header and verify its finality and the intervening committee handoffs. Save its encoded Header as a JSON hex string. Prepare a request with `epoch_record` and `proof` from the query, the historical `registry` bytes, the applied `committee` bytes and a `subject`. For a validator, the subject contains `kind: "validator"`, its `validator` public key and Stake Cell `metadata`. For delegation it contains `kind: "delegation"`, the 128-byte `identity`, integer `principal` and historical sparse `proof` collected from the positions at the epoch's Registry snapshot.

```sh
stem stake reward-quote --network "$STEM_NETWORK" --header header.json --request reward.json
```

The response gives the CKB recipient Lock hash, share, commission and stable position ID with its role. Build a claim with integer `payout` in shannons, `recipient` as the complete CKB JSON Lock, and an `epochs` list. Each entry contains `epoch`, the historical `registry` and `committee` bytes and `subjects` in the same shape as the quote request. All subject hashes must match the provided Lock, which must be supported by the network's withdrawal policy. A claim can combine up to eight epochs and 32 positions.

Use `payout: 0` with new positions to accrue earnings without allocating a withdrawal nonce. To redeem only an accrued balance, set `payout` to the full balance and provide `epochs: []`. A claim may also add new earnings and redeem the full resulting balance in the same operation. Redemption requires the Lock's CKB occupied Capacity and sufficient unreserved capacity in the native vault.

```sh
stem stake claim-rewards --request claim.json
```

Broadcast the resulting transaction. Query `/stem/stake-reward-balance` with `prove=true` and the 32-byte recipient Lock hash. Verify the response against its authenticated finalized Header using `stem stake verify-reward-balance --network NETWORK --owner LOCK_HASH --header header.json --proof balance.json`. If the transaction requests a payout, relay the pending receipt through the ordinary checkpoint and fund payout flow. For guardian reconciliation following a committee change, supply the epoch handoff proofs along with the finalized execution proof.

To check one position, query `/stem/stake-reward-claim` with `prove=true` and concatenate the little-endian eight-byte epoch, one-byte role and 32-byte position ID. A present record holds the 32-byte accrual transaction ID and the little-endian u64 amount. Authenticate it with `Store::verify_stake_reward_claim`. Audit actual L1 payment and Stem settlement by the native lane nonce. [Redeem fee revenue on CKB](#redeem-fee-revenue-on-ckb)

To exercise the complete verification path on devnet, use the same command and contract paths as the delegation exercise, with a fresh output directory and these additional options.

```sh
  --reward-accounting --vault "$native_dir/vault-type" \
  --deposit-lock "$native_dir/deposit-lock" \
  --native-lock "$native_dir/stem-lock" --wrapped "$native_dir/bridge-udt"
```

The runner bridges actual CKB, pays the experimental 800 CKB fee, verifies the closed epoch and validates each historical entitlement. It accrues two awards for a delegation beneficiary in separate transactions, restarts the active archive replica and verifies the recovered balance before redeeming it without repeating the historical proofs. It rejects a forged principal, a tampered balance proof and a payout calculated from a stale balance. Use a fresh development home for the genesis that commits accrued stake liabilities.

## Exercise CKB Checkpoint publication

Add `--checkpoint "$contracts_dir/checkpoint"` to the native stake runner. It builds an archive index, verifies signed Stem successors and committee handoffs, and prepares two Checkpoint Type ID transactions for the CKB devnet. The update rotates the checkpoint authority with both quorums. The runner also submits invalid transactions to check that CKB rejects a minority certificate, missing rotation signatures and a corrupted history extension.

For independent publication, use an archive Stem replica started with `--archive`. Capture its `/stem/stake-snapshot` response with `prove=true`, write the decoded proof JSON, and obtain the signed successor at the returned height plus one. The deployment configuration pins the Stem network, CKB genesis, Checkpoint Type Script and initial authority generation. Run `stem checkpoint prepare --help` for the proof bundle inputs. For an update, supply the confirmed predecessor's canonical OutPoint and Cell Data. The resulting signing message binds the pinned domain, actual predecessor and canonical Cell record. Verify the predecessor and its confirmations on a trusted CKB node before authorizing publication.

The CKB Type Script uses Data2 with the 32-byte Type ID, 32-byte Stem network, 32-byte CKB genesis and 32-byte initial authority generation, in that order. The Lock uses the same code with the 32-byte complete Type Script hash followed by zero. Put the encoded publication certificate in the input WitnessArgs `input_type` of the checkpoint input, or of input zero when creating the Cell. The certificate starts with the little-endian u16 MMR suffix count and corresponding hashes. Then encode the incumbent authority and signatures. For each authority, write a u16 member count, sorted 32-byte Ed25519 keys with little-endian u64 weight, a u16 vote count, and sorted u16 signer indices with 64-byte signatures. A rotation appends a second authority certificate. The L1 script enforces a maximum of 16 members for each authority.

Use `stem checkpoint verify` with pinned trust parameters before broadcasting through the CKB wallet. `stem checkpoint history` indexes committed Headers in a separate RocksDB database and produces an MMR inclusion path. Authenticate the target Header's history root before consuming its proofs. The archive index needs retained data from genesis. CKB depth and reorganization monitoring are the publisher's responsibility.


## Exercise the direct CKB checkpoint

Compile the L1 contract for CKB-VM and build the Go proof exporter.

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf --target-dir target/stem-contracts \
  --bin direct-checkpoint --bin stake-registry
mkdir -p target/stem-tools
go -C tools/proof build -mod=readonly -o ../../target/stem-tools/stem-proof .
cargo build --locked --release --bin stem
python3 tools/native_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --comet target/stem-tools/cometbft \
  --stake-registry target/stem-contracts/riscv64imac-unknown-none-elf/release/stake-registry \
  --proof-verifier target/stem-tools/stem-proof \
  --direct-checkpoint target/stem-contracts/riscv64imac-unknown-none-elf/release/direct-checkpoint \
  --output target/stem-runs/direct-checkpoint
```

The runner derives the native genesis bytes with
`stem genesis-hash --genesis SPEC --header`. For each successive height,
collect the public Comet `commit` response, every page of `validators` at
that height and the preceding native Header. The runner checks the
height, total and membership across pages before assembling the input.
`stem-proof -genesis COMET_GENESIS -direct-bundle SOURCE`
exports a JSON object containing the witness and its expected tip.
`stem checkpoint verify-direct --help` lists the trust and previous-tip
arguments for off-chain verification. The actual publication places the direct
proof in `WitnessArgs.input_type` and the verified tip in the unique Cell.
The test output directory retains the individual bundles, estimated cycles
and L1 transaction-and-witness inclusion proofs.

## Exercise standalone CKB Metatype

Build the dispatcher and the two independently deployed adapters:

```sh
CARGO_TARGET_DIR=target/stem-metatype \
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf \
  --bin metatype --bin metatype-udt --bin metatype-transfer-policy
```

Then use a CKB IntegrationTest binary and a fresh output directory:

```sh
contract_dir=target/stem-metatype/riscv64imac-unknown-none-elf/release
python3 tools/metatype_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --dispatcher "$contract_dir/metatype" \
  --udt "$contract_dir/metatype-udt" \
  --policy "$contract_dir/metatype-transfer-policy" \
  --l1-port 30420 --output /tmp/stem-metatype-ckb
```

The fixture deploys three RISC-V ELFs and an immutable adapter table.
It checks issuance, transfers under heterogeneous manifests, multiple
assets in one output, and contract error codes for rejected
transitions. The report includes CKB inclusion proofs, cycles,
transaction bytes, witness payload sizes, actual occupied Capacity
and local submission-to-mining timing. The current fixture uses a
standalone CKB deployment. See the [Metatype specification](METATYPE.md)
for limits, adapter review and the cross-chain requirements.

Repeat with `--shared-manifests`, a fresh output directory and a
different L1 port to measure the same inputs and outputs using
shared definitions. The fixture publishes the definitions, spends
one to replace it at a new OutPoint, and verifies a mixed-layout
atomic transfer. It includes negative controls for unavailable or
malformed definitions and the expanded transaction budget. Retain
the exact definition bytes to republish them if a live dependency
disappears.

For the borrowed-Manifest comparison, pin the three RISC-V ELFs
from each revision. Run each set with inline and shared data, then
repeat the four runs in reverse order. Keep the fixture and its
native Stem binary fixed. The report records the activated
deployment, cycles, transaction bytes and inclusion for each split.

### Exercise immutable objects and independent approvals

Include the following ELFs in the RISC-V build:

```sh
CARGO_TARGET_DIR=target/stem-metatype \
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf \
  --bin metatype --bin metatype-object --bin metatype-object-approval \
  --bin stem-lock --bin stem-native-object --bin stem-object-policy

contract_dir=target/stem-metatype/riscv64imac-unknown-none-elf/release
python3 tools/metatype_object_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --dispatcher "$contract_dir/metatype" \
  --object "$contract_dir/metatype-object" \
  --approval "$contract_dir/metatype-object-approval" \
  --native-lock "$contract_dir/stem-lock" \
  --native-object "$contract_dir/stem-native-object" \
  --native-policy "$contract_dir/stem-object-policy" \
  --port 30450 --l1-port 30440 --output /tmp/stem-metatype-objects
```

The first CKB Header Dep supplies the genesis Header pinned in the
deployment args. The owner signs with the CKB Sighash Lock after the
independent approvals have been placed in the first extra witness.
The fixture writes CKB inclusion proofs, proposal and receipt
evidence from four native replicas, and each negative case. Use
the three native script options together for the differential run.

### Exercise Stem to CKB Metatype custody

Build the existing Jolteon checkpoint, CKB vault, Intercell mailbox,
and native object scripts together with the immutable custody
profile:

```sh
CARGO_TARGET_DIR=target/stem-metatype \
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf \
  --bin vault-type --bin deposit-lock --bin stem-lock --bin bridge-udt \
  --bin jolteon-checkpoint --bin intercell-mailbox \
  --bin stem-native-object --bin stem-object-policy --bin stem-metatype-escrow \
  --bin metatype --bin metatype-mailbox \
  --bin metatype-bridged-object --bin metatype-bridged-approval

contract_dir=target/stem-metatype/riscv64imac-unknown-none-elf/release
python3 tools/jolteon_bridge_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --vault "$contract_dir/vault-type" --deposit-lock "$contract_dir/deposit-lock" \
  --native-lock "$contract_dir/stem-lock" --wrapped "$contract_dir/bridge-udt" \
  --direct-checkpoint "$contract_dir/jolteon-checkpoint" \
  --intercell-mailbox "$contract_dir/intercell-mailbox" \
  --stem-native-object "$contract_dir/stem-native-object" \
  --object-policy "$contract_dir/stem-object-policy" \
  --stem-metatype-escrow "$contract_dir/stem-metatype-escrow" \
  --metatype-dispatcher "$contract_dir/metatype" \
  --metatype-mailbox-lock "$contract_dir/metatype-mailbox" \
  --metatype-bridged-object "$contract_dir/metatype-bridged-object" \
  --metatype-bridged-approval "$contract_dir/metatype-bridged-approval" \
  --port 31780 --l1-port 31790 --output /tmp/stem-metatype-custody
```

To run the specialized comparison, replace the five custody and
dispatcher options with `--ckb-stem-object "$contract_dir/ckb-stem-object"`
and build that ELF. Both profiles use the same native object and
independent approval fixture. Use a fresh output directory and
ports for every run. The output includes the original and returned
Cell proofs, each CKB transaction and witness inclusion proof, and
all accepted and rejected signed transactions.

Pass `--metatype-shared-manifest` to publish the original Script set
as a separate CKB definition Cell and use its hash in the custody
representation. The same fixture then rejects an import that omits
that dependency.

Add `--metatype-revisable-object` to the Metatype command to issue a
revisable object. The fixture starts at revision 0, confirms revision 1
on Stem, exports it, and authorizes revisions 2 and 4 on CKB. It returns
the object at revision 4 and checks the original Capacity and both
Type Scripts. The same fixture rejects missing approval, a same-version
edit, rollback and stale return state. Run with and without
`--metatype-shared-manifest` for the two storage layouts. Use a distinct
output directory and fresh ports for each layout.

Build `stem-object-royalty` and `metatype-object-royalty` with
the same RISC-V command. Add
`--native-object-royalty "$contract_dir/stem-object-royalty"` and
`--metatype-object-royalty "$contract_dir/metatype-object-royalty"`
to the Metatype custody fixture. The fixed payment is 110 CKB in
this experiment. The fixture confirms a paid Lock change on each
chain and verifies the three Stem payments for transfer, export and
release. Run again with a shared Manifest or a revisable object.
Inspect `native_object.royalty` for the payee Locks, payment
markers, finality proofs and CKB output index.

Build `metatype-approval-registry`, `stem-object-rotating-approval`
and `metatype-object-rotating-approval` with the same RISC-V command.
Pass their paths via `--metatype-approval-registry`,
`--native-object-rotating-approval` and
`--metatype-object-rotating-approval`. The runner registers an
Ed25519 quorum on CKB, advances Stem's anchor and exports with
that quorum. It rotates the roster while the object is on CKB,
then transfers, burns and releases using the next generation.
It captures the registry transactions in `native_object.approval_rotation`.
The same fixture can include a live capacity rule on a revisable object.

Use `--metatype-approval-quorum 2 3 --stem-transaction-cycles 28000000`
to exercise two signatures from a three-key roster. The runner also
checks insufficient, repeated, out-of-order and wrong-network signatures
on both chains. Use the same explicit genesis limit in the one-signer
control with `--metatype-approval-quorum 1 1`. The runner's default
transaction limit is 20,000,000 cycles.

Add `--metatype-object-freeze` to the rotating approval fixture.
It freezes the object before a CKB burn, thaws for the burn, then
freezes again before Stem release. It verifies that the failed release
leaves the escrow live and the Inbox untouched. It advances Stem
to the thawed record and releases the same message with a fresh
approval. See `native_object.freeze` in the report.

Add `--metatype-revision-topup` to test an update that exceeds the
original Stem Cell's storage budget. The original asset holds 8 CKB.
The signed return adds 5 CKB and keeps the exact authenticated state
and both original Type Scripts. The CKB fixture starts with a
1,000 CKB mailbox and tops it up to 3,300 CKB on import. The gate
rejects an authenticated import leaving insufficient capacity for its
largest valid return. Mailbox funding remains locked in the mailbox.

For the three-Script object, build `stem-object-state-limit` and
`metatype-object-state-limit` with the same RISC-V command. Add
`--native-object-state-limit "$contract_dir/stem-object-state-limit"` and
`--metatype-object-state-limit "$contract_dir/metatype-object-state-limit"`
to the revisable object command. The fixture publishes the same
120-byte ceiling record on both chains. It rejects the same changed
object state on both chains, rejects missing and substituted records,
and verifies the restored state at the exact byte limit. It also
spends the original CKB record and republishes the same bytes at a
different OutPoint while the object is on CKB. Use distinct ports and
outputs for inline and shared Manifests. Check `native_object.external_rule`
in the report for both CKB inclusion proofs and the authenticated
Stem record. Combine the rule with `--metatype-revision-topup` to raise
the fixture's rule limit to 700 bytes. This run starts at 9 CKB and
adds 5 CKB to restore all three Scripts and the final state.

Build `metatype-rule-registry` and add
`--metatype-rule-registry "$contract_dir/metatype-rule-registry"` to
the revisable three-Script command to exercise a live rule. The
CKB registry Type ID stays fixed while the two-byte record changes
from 120 to 200. The run advances Stem's CKB anchor twice and obtains
guardians' live rule observations. It verifies that an old reference
cannot authorize a return under the new anchor. The historical mailbox
record and live registry record share one certificate, so the return
fits the usual 20-million-cycle transaction limit.

Add `--metatype-decrease-ceiling` to that fixture to lower the registry
while an import and return are pending. It preserves the bytes of
already admitted state, checks the oversized boundary proofs, rejects
an oversized local edit and then commits a revision within the lower
ceiling. Inspect `native_object.ceiling_decrease` for the signed
registry updates, route proof size and finality evidence.

Use an archival CKB RPC to reconstruct Proof Data for an oversized
Metatype return.

```sh
stem ckb-wallet recover-metatype-ceiling \
  --request ceiling-recovery.json --l1-rpc "$CKB_RPC"
```

The request contains `escrow_cell`, `ceiling_script_hash`,
`stem_network`, `burn` and `anchor`. Supply the canonical Cell
bytes from an authenticated Stem escrow proof. The `burn` is the CKB
JSON OutPoint of the mailbox successor. Use a trusted CKB block
hash at or after that burn for `anchor`. The CLI verifies the exact
burned object, mailbox, registry dependency and pinned adapter table.

Place the returned `proof_data` in the ceiling Type Group before
calculating approvals and signing Locks. Attach the normal
historical mailbox and current live registry certificate at Stem's
committed anchor. The CLI's `ceiling_at_burn` identifies the
historical record resolved by CKB. Recovery applies the same
4,096-Header, 512-transaction and 32 MiB retrieval bounds as
workflow recovery.

Build `stem-object-append-only` and `metatype-object-append-only`
and add the following options to the revisable object command.
Keep the two state-limit options and the registry option to test all
four constraints together.

```sh
--native-object-append-only "$contract_dir/stem-object-append-only" \
--metatype-object-append-only "$contract_dir/metatype-object-append-only"
```

The fixture appends content through revisions 1, 2 and 4, then
verifies the restored object and exact original Type Script set.
It submits matched rewrite, truncation, MIME-change and version-only
controls to Stem and CKB before export and before the first CKB
revision. The report preserves the common state inputs and outputs
under `native_object.append_only`. Run with and without
`--metatype-shared-manifest`, using distinct output directories
and ports. Omit the two append-only binaries for a matching
three-constraint control with the same valid content and revision
trajectory.

To exercise signed intermediate history, build
`stem-object-workflow` and `metatype-object-workflow` and add
`--native-object-workflow "$contract_dir/stem-object-workflow"` and
`--metatype-object-workflow "$contract_dir/metatype-object-workflow"`
to the revisable object command. The workflow fixture uses its own
object data and is run separately from the length and append-only
fixtures. Run it with and without `--metatype-shared-manifest`.
The report records the request at revision 1, separately confirmed
CKB revisions 2 and 3, and the two signatures attached to the Stem
return witness. `--metatype-workflow-early-return` returns at revision
2. Both variants perform an ownership transfer with unchanged state.

Use an archival CKB RPC to recover Proof Data after restart.

```sh
stem ckb-wallet recover-metatype-workflow \
  --request workflow-recovery.json --l1-rpc "$CKB_RPC"
```

The request has `escrow_cell`, `workflow_script_hash`, `import`,
`burn` and `anchor` fields. Use the canonical Stem Cell bytes
from an authenticated escrow record, as in a wallet signing
context. `import` is the original CKB object output and `burn`
is the CKB mailbox successor, both as CKB JSON OutPoints. The
anchor is an independently trusted CKB block hash at or after
the burn. The tool checks that the RPC still serves this anchor
on its canonical chain before and after recovery.

The output includes the exact Intercell `record`, chronological
OutPoint trace, return state and `proof_data`. Place those proof
bytes in the workflow Type Group's wallet build request, and attach
the normal guardian certificate and required Lock signatures for
the return. Recovery reads up to 64 custody transactions and
4,096 parent Headers, with at most 512 fetched transactions and
32 MiB of packed CKB data. Large or pruned histories require
retained CKB data and the original bounded workflow signatures.

### Exercise cap-bound Metatype fungible custody

Build and run the fungible Metatype profile against CKB devnet with
four Stem replicas.

```sh
CARGO_TARGET_DIR=target/stem-metatype \
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
cargo build --locked --release --manifest-path contracts/Cargo.toml \
  --target riscv64imac-unknown-none-elf \
  --bin vault-type --bin deposit-lock --bin stem-lock --bin bridge-udt \
  --bin jolteon-checkpoint --bin intercell-mailbox --bin stem-native-udt \
  --bin stem-amount-cap --bin stem-metatype-escrow --bin metatype \
  --bin metatype-mailbox --bin metatype-udt --bin metatype-amount-cap

contract_dir=target/stem-metatype/riscv64imac-unknown-none-elf/release
python3 tools/jolteon_bridge_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --vault "$contract_dir/vault-type" --deposit-lock "$contract_dir/deposit-lock" \
  --native-lock "$contract_dir/stem-lock" --wrapped "$contract_dir/bridge-udt" \
  --direct-checkpoint "$contract_dir/jolteon-checkpoint" \
  --intercell-mailbox "$contract_dir/intercell-mailbox" \
  --export-udt "$contract_dir/stem-native-udt" \
  --native-amount-cap "$contract_dir/stem-amount-cap" \
  --stem-metatype-escrow "$contract_dir/stem-metatype-escrow" \
  --metatype-dispatcher "$contract_dir/metatype" \
  --metatype-mailbox-lock "$contract_dir/metatype-mailbox" \
  --metatype-udt-adapter "$contract_dir/metatype-udt" \
  --metatype-amount-cap-adapter "$contract_dir/metatype-amount-cap" \
  --metatype-shared-manifest --finish-udt-return \
  --port 33030 --l1-port 33040 --output /tmp/stem-metatype-fungible
```

The fixture exports 250 units with a 200-unit limit per Cell.
The standard run returns 150 units. `--finish-udt-return` then returns
the remaining 100 through the next mailbox nonce. The separate
`--full-udt-return` run returns all 250 in the first burn. Each
redemption uses an independently authenticated CKB anchor. Omit
`--metatype-shared-manifest` for inline script definitions. The report
contains canonical CKB inclusions, Stem finality and execution proofs,
and membership or absence proofs for escrowed and released Cells.

Replace `--finish-udt-return` with `--metatype-multiasset` to settle
two independent capped assets. The fixture exports, mints, swaps
ownership, burns and returns both assets in one transaction per stage.
Each CKB mailbox takes its own delivery witness. A single Stem guardian
certificate covers both historical burn outputs. Use fresh ports and
output directories for inline and shared runs. The report preserves
both message IDs, the finalized execution receipts and the proof of
each released Cell. The fixture also submits a separate four-action
transaction and checks rejection of a fifth action.

Use `--metatype-composed-cell` in place of `--metatype-multiasset`
to compose two capped assets in one Cell and settle that Cell through
one mailbox. The fixture issues two scalar Cells, combines them, runs
the CKB custody stages, and splits the returned Cell into its original
assets. Run with and without `--metatype-shared-manifest` and use fresh
ports and output directories. Add `--metatype-composed-partial` to
redeem 60 and 110 units and verify the residual 100 and 140 units on
both chains. Use `--metatype-composed-selective partial` to redeem
60 units of the first asset while keeping all 250 units of the second.
Use `--metatype-composed-selective exhaust` to redeem all 160 units of
the first asset and then redeem the second via the next CKB mailbox
sequence. The runner checks rejection of an all-zero representation.


### 创世时启用 StemVM 密码学 ABI v1

新网络可在顶层 genesis JSON 中加入 `"vm_crypto_v1": true`，并让所有节点核对 `stem genesis-hash --genesis genesis.json --header` 的结果。节点会拒绝用另一种配置打开原数据库。现有网络沿用已有配置。开发网脚本 `tools/jolteon_localnet.py --vm-crypto-v1` 可用相同的权限规则演练停机追赶。接口、返回码和预算见 [StemVM 协议](STEM-PROTOCOL.md)。

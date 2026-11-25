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

The Stem genesis JSON requires `chain_id`, a 32-byte `network_id`, `genesis_time_ms`, storage-bond terms, and explicit byte, cycle, read and write limits. A stake-backed genesis supplies `stake.trust` and `stake.observations` from CKB guardians, a signing window, and snapshot limits. The `development_validators` and `development_cells` fields are for fixture networks. A bridged spec pins the CKB genesis and anchor, the vault Type IDs and initial OutPoints, wrapped code, payout Lock templates, and the bridge guardian set.

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

For paired partial authorization, fund one Cell using the `stem-intent-lock` Data code hash and 52 bytes of arguments. The first 20 bytes are the owner's compressed-key Blake160, followed by a 32-byte locally unique salt. Choose the exact delivery Cell and resolve the common Cell and Header Deps before signing. Use `stem wallet intent sign` with the signing-context JSON and a draft whose selected input and delivery are at the same index. The signer supplies an exclusive `--valid-before-height` and `--max-fee` in shannons. A sponsor additionally supplies `--sponsor-max-debit` and puts plain change at index 0 and the exact sponsored pair at index 1. The solver builds a new draft with every pair and uses `stem wallet intent attach --intent first.json --intent second.json`. Collect any complete transaction signatures after attaching the terms. To cancel, consume the input with `stem wallet sign --intent-code /path/to/stem-intent-lock` and empty proof data for that Lock Group.

To enable Lineage lifecycle transitions in a fresh network, set `lineage_type` to the Data hash of the compiled `stem-lineage` RISC-V ELF and publish that program as a Cell Dep. Use `stem_ledger::lineage::id` and `marker` to build the Type Script. Register from an ordinary input at index 0 using group Action 0 with a 32-byte Executor Data hash. Use Action 1 for code replacement and Action 2 with empty data for retirement. Query `/stem/lineage` with a 32-byte Lineage ID and `prove=true`. Decode the Strand registry `record`, its `proof`, the `cell` and `cell_proof`. Verify both paths with `LineageProof::verify` against the same authenticated Header. Registration, Executor replacement and retirement use explicit OutPoints.

For dynamic commands, publish the registered Executor and the example `stem-lineage-lock` as Cell Deps. Prepare a head with `stem_ledger::lineage::lock` in head mode. Fund a separate plain CKB Cell with the same helper in fee mode. Use `Transaction::new_lineage_command` with the fee OutPoint, ordered target IDs and version ranges, Executor Data hash and OutPoint, signed fee, execution budgets, application payload and exclusive expiry. Sign `Transaction::lineage_signing_digest` with the fee Cell owner's secp256k1 key and place its 65-byte seal at Group Witness index 0. Use the ordinary node broadcast interfaces. The client must authenticate the fee Cell and check the Executor and application payload before signing. Owners can still authorize exact Head transitions with a complete transaction signature.

The CLI builds and signs the same native command. The build request has
`fee_input`, `cell_deps`, `targets`, `executor`, `executor_dep`,
`valid_before_height`, `executor_cycles`, `validation_cycles`, `fee`
and hex `payload` fields. A target contains `id`, `min_version`,
`max_version` and `min_capacity`. Order targets by raw ID bytes. The
signing context includes the authenticated fee and Executor Cells. Supply
a separate expected Executor hash and a local fee cap before signing.

```sh
stem wallet lineage build --request command.json > draft.json
stem wallet lineage sign --genesis stem-genesis.json --transaction draft.json \
  --context cells.json --code stem-lineage-lock --key-file fee-key \
  --executor-hash "$EXPECTED_EXECUTOR" --max-fee "$FEE_CAP" > command.signed.json
```

For an ordinary exact spend of a Head or fee Cell, use
`stem wallet sign --lineage-code stem-lineage-lock` with the usual
transaction, Lock, context and key arguments.

Use `lineage::Outcome::decode` on the authenticated receipt protocol result. Position 0 always refers to fee change. On success each ordered target's output follows it. On a fee-only failure no target outputs are created. The four-node runner exports `lineage-commands.json` containing the real command results and the state and certificate evidence.

**Sponsored Lineage intent.** Use `stem wallet lineage actor --key-file KEY` to derive each business ID. Include `"business":{"actors":[…],"max_fee":2000}` with sorted actor IDs in the Lineage build request. Bind at least one target to an exact version. The business draft may omit `fee_input` and `fee`. Each party reviews the full business payload and uses `stem wallet lineage sign-business --genesis SPEC --transaction DRAFT --executor-hash HASH --key-file KEY`. The collector then runs `stem wallet lineage assemble --genesis SPEC --transaction DRAFT --fee-input OUTPOINT_JSON --fee SHANNONS --seal PARTY_A_JSON --seal PARTY_B_JSON`. Only the fee payer seals the assembled transaction with `stem wallet lineage sign`. For Keeper, set `execute_at` before collecting the business seals and reserve the fully authorized transaction.

**Keeper.** Enable `keeper` in the genesis JSON of a Lineage-enabled network. The research fixture uses `{"max_delay":64,"per_height":4,"reservation_fee":2000}`. Choose limits such that the configured schedule fits the block. Add an `execute_at` height earlier than `valid_before_height` to the Lineage build request, then sign the command as above. Include the resulting hex string under `"keeper": {"schedule": "0x…"}` in an ordinary wallet build request and sign that carrier with its own funding inputs. Fees must cover the reservation charge and ordinary resource prices. Broadcast the carrier. The signed command becomes executable only through the committed queue.

To cancel before its due height, build a transaction consuming the reserved fee input and include `"keeper": {"cancel":{"height":42,"fee_input":…}}` with the actual height and OutPoint. Sign with the fee Cell's Lock. If a normal spend consumes the fee Cell first, its pending reservation is skipped at the due height. Queries to `/stem/keeper` take an eight-byte little-endian height and `prove=true`. For completed attempts, use `/stem/execution`. At a due height, attempt indices precede submitted transaction indices. Triggering depends on the network continuing to produce blocks.

`stem wallet build --request request.json` encodes inputs, outputs, dependencies and optional ordered `messages` and `proofs`. `stem wallet sign` needs the native genesis, complete requested Lock Script, group index and `--context` file. Its `cells` array contains `{"out_point": ..., "cell": "0x..."}` records for all inputs, explicit dependencies and expanded Dep Group members. Its optional `headers` array contains canonical encoded Header Deps. Obtain and authenticate these records against the expected chain state before signing. `stem wallet id --network HASH` reads the JSON hex transaction from stdin. `stem ckb-wallet` uses the CKB-native encoding for L1 signing.

A separate payer can sign its Lock Group with `--sponsor-max-debit SHANNONS` after the participants fix the full transaction. The wallet requires plain Capacity inputs and change for that group. It checks that the payer's net debit fits the supplied cap and the actual total transaction fee. Each signer authorizes the same transaction and can add its seal independently.

`stem wallet find` discovers by complete Lock or Type Script hash in a local, read-only application database. Repeat the returned height and cursor for a consistent page sequence. Query selected OutPoints through ABCI for corresponding authenticated proofs.

With `prove=true`, `/stem/cell` returns a hex Strand `PatriciaPath` in its `proof` field. `/stem/execution` returns a hex Strand `ExecutionProof` with one position and count for both Merkle paths. Verify each proof against the corresponding root in the authenticated Stem Header.

Query `/stem/extinguished-fees` with `prove=true` and empty data to audit the cumulative fee tally at the current application height. The response contains `extinguished_shannons` as a hex u64 or `null` for an absent record, and a Strand Patricia proof. Authenticate the Header at the returned height through its signed successor, then verify against its SystemRoot using `Store::verify_extinguished_fees`. A verified absent record represents zero fees. The tally measures extinguished Capacity and can include fees from development allocations.

On a bridged network, `/stem/bridge-backing` with `prove=true` and empty data returns a Strand `record` and `proof`, plus `liquid_shannons`, `circulating_shannons`, `pending_shannons` and `unallocated_shannons` as hex u64 values. Decode the record with `stem_ledger::protocol::BridgeBacking::decode` and the proof with `stem_ledger::patricia::Proof::decode`. Verify the record using `Store::verify_bridge_backing` with the trusted network identity and Header at the returned height. Use the verified record for numerical values. CKB vault liveness and confirmations follow the guardian observation model.

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

The vault Type Script args are five 32-byte values in order. They encode the vault Type ID, direct checkpoint code hash, direct checkpoint Type ID, CKB genesis hash and Stem network ID. Use the intended checkpoint creation input and output index to commit its Type ID before writing the Stem genesis. The checkpoint bootstraps with the resulting Stem genesis AppHash. Keep the seed under deployment control through bootstrap, and pin the committed checkpoint's genesis committee, Stem genesis AppHash, chain ID and network before accepting deposits. Vault locks use the same executable with the vault Type Script hash followed by the 20-byte CKB multisig identifier. Consolidation, reserve movement and rotation require a separate input authorized by that multisig.

The runner also reconciles the recognized balance and liabilities against the amounts minted and paid on both lanes. The final report includes the ABCI backing record and path. Store and snapshot tests independently authenticate the path and reject altered amounts.

The runner saves the packed CKB payout transaction and its header with the dual Merkle proof. A wallet or relayer can validate a historical output offline with an independently trusted CKB block hash.

```sh
stem ckb-wallet verify-inclusion --transaction ckb-payout-packed.json \
  --proof ckb-payout-inclusion.json --block-hash "$confirmed_block_hash" \
  --tx-hash "$payout_transaction_hash" --output-index 0
```

The result authenticates the selected output at creation. It provides no authorization to consume the output or evidence about its later spend status.

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

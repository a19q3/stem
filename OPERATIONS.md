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

Compute `stem genesis-hash --genesis stem.json`. Set CometBFT `app_state.stem_genesis` to this hex value without the `0x` prefix, set the same chain ID and genesis time, and use the exact Stem genesis committee. Start Stem in a private home and set CometBFT's ABCI address to the absolute socket path.

```sh
target/release/stem run --home /path/to/stem-home --genesis stem.json --workers 2
target/stem-tools/cometbft start --home /path/to/comet-home \
  --proxy_app unix:///path/to/stem-home/abci.sock
```

Ordinary replicas prune application execution bodies and Header indices and request CometBFT block pruning after `--history-window`. The window must cover Header Deps and the stake signing window. CometBFT retains metadata required for its own evidence deadlines. Serve older executions, handoff bundles and CKB anchor proofs from an archive replica. `--archive` preserves application bodies and Headers as they arrive. Snapshots export canonical state records and a recent Header suffix. State-sync consumers authenticate the AppHash through CometBFT's light client and rebuild the two state roots before installing. Configure `[statesync]` on a fresh observer with two reachable RPC servers, a trusted signed block height and hash, and reachable P2P peers. `--snapshot-interval` controls asynchronous exports and `--snapshot-bytes` caps the received artifact size.

## Operate a replica under systemd

The `ops/systemd` units run the native ABCI app and CometBFT under one target. Put both binaries on the host, initialize a distinct CometBFT home per replica, and give each instance a private Stem home and a `node.env` derived from the example. Set `ROLE_ARGS=--archive` on archive replicas. Install the supplied unit files for the service user and start the target for the absolute directory containing `node.env`.

```sh
instance=$(systemd-escape --path /srv/stem)
systemctl --user start "stem@${instance}.target"
journalctl --user -u "stem-app@${instance}.service" -u "stem-comet@${instance}.service"
```

Keep CometBFT's private validator key and signing state together during recovery. Each key must have a single signing process. Back up the Stem specification, CKB guardian trust and CometBFT signing state independently of disposable state-sync snapshots. Enable CometBFT's Prometheus instrumentation on an operator interface when collecting consensus metrics.

## Exercise the native CometBFT path

```sh
cargo test --locked -p stem-ledger encoding_tests -- --nocapture
cargo test --locked -p stem-vm --test native -- --nocapture
STEM_TEST_COMET=target/stem-tools/cometbft \
  cargo test --locked --test native_store four_node_native_finality_restart_and_app_hash \
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
  --proof-verifier target/stem-tools/stem-proof --output target/stem-runs/native-bridge
```

Add `--xudt /path/to/xudt_rce --xudt-gate "$contracts_dir/xudt-gate"` to deploy a separate xUDT vault and execute both asset round trips. The runner uses isolated CKB and CometBFT nodes and disposable key files. At mint, guardians audit the actual confirmed consolidation and sign the exact native output bytes. To reconcile a payout, they authenticate the Stem execution receipt and the CKB transaction that matches its recipient, quantity and vault nonce.

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
python3 tools/stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb \
  --stake-registry "$contracts_dir/stake-registry" --output target/stem-runs/ckb-stake
python3 tools/native_stake_localnet.py \
  --binary target/release/stem --l1 /path/to/ckb --comet target/stem-tools/cometbft \
  --stake-registry "$contracts_dir/stake-registry" \
  --proof-verifier target/stem-tools/stem-proof --output target/stem-runs/native-stake
```

The first runner checks the L1 contract's registration, increase, freeze, retirement, penalty, withdrawal and guardian rotation. The second uses actual L1 observations to initialize a CometBFT committee, add and remove a validator at H+2, verify both signed handoffs, and unbond the removed validator. Use the emitted evidence directories to inspect transaction proofs and exact block heights.

Use `stem stake retirement-proof` with the retired public keys from confirmed CKB Registry history. Compare its returned root with the current Registry Cell. A registration supplies the absence proof after the 64-byte registration signature. A delayed withdrawal supplies the proof as its operation payload and installs the returned next root. The L1 Stake Cell and Registry are consumed atomically. The old public key stays retired after the active slot is released.

`stem stake authorize-retirement` signs the next Registry state after checking an authenticated `/stem/stake-retirement` proof, the signed successor's committee, and the actual CKB Stake Cell. The L1 Stake Registry accepts the certificate only from the current guardian quorum. For a change of guardians, `stem stake observe` accepts the previous confirmed Registry OutPoint and outgoing committee in the rotation request. Collect observations from both sets before submitting the native snapshot.

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

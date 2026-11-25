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

The four-node test prints its evidence directory. It generates a fresh network, starts four validators, exercises signed payments and a two-Type-Script transfer, restarts a replica and joins a fifth node using remote snapshots. It leaves the test configuration, logs, headers and commit certificates in the evidence directory.

```sh
STEM_NATIVE_EVIDENCE=/path/to/printed/evidence \
  go -C tools/proof test -mod=readonly -run TestNativeCommitSignatures -v .
```

The benchmark uses the same four-node harness. Set `STEM_TEST_LOADGEN`, `STEM_TEST_LOAD_RATE`, `STEM_TEST_LOAD_COUNT` and `STEM_TEST_LOAD_KIND`. Workloads are `payment`, `single`, `double`, `multi` and `settlement`. Set `STEM_TEST_EVIDENCE_ROOT=target/stem-runs` to keep evidence on the workspace filesystem. Set `STEM_TEST_VALIDATORS` to vary the same-host benchmark from 4 to 32 replicas. Use `--release` for throughput characterization.

## Wallet and proofs

`stem wallet build --request request.json` encodes inputs, outputs, dependencies and optional ordered `messages` and `proofs`. `stem wallet sign` needs the native genesis, complete requested Lock Script, group index and `--context` file. Its `cells` array contains `{"out_point": ..., "cell": "0x..."}` records for all inputs, explicit dependencies and expanded Dep Group members. Its optional `headers` array contains canonical encoded Header Deps. Obtain and authenticate these records against the expected chain state before signing. `stem wallet id --network HASH` reads the JSON hex transaction from stdin. `stem ckb-wallet` uses the CKB-native encoding for L1 signing.

`stem wallet find` discovers by complete Lock or Type Script hash in a local, read-only application database. Repeat the returned height and cursor for a consistent page sequence. Query selected OutPoints through ABCI for corresponding authenticated proofs.

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

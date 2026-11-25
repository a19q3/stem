# Fern operations

## Build

Use the pinned Rust toolchain, the `riscv64imac-unknown-none-elf` Rust target, Clang and LLD, OpenSSL development headers, Go, Python and the Python `cryptography` package. Local L1 tests require a CKB executable with the IntegrationTest RPC module.

```sh
cargo build --locked --release -p fern
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --manifest-path contracts/Cargo.toml --release \
  --target riscv64imac-unknown-none-elf
mkdir -p target/fern-tools target/fern-runs
GOBIN="$PWD/target/fern-tools" go install github.com/cometbft/cometbft/cmd/cometbft@v0.38.26
go -C tools/proof build -mod=readonly -o ../../target/fern-tools/fern-proof .
go build -o target/fern-tools/fern-loadgen tools/loadgen/main.go
```

## Start a native replica

The Fern genesis JSON requires `chain_id`, a 32-byte `network_id`, `genesis_time_ms`, storage-bond terms, and explicit byte, cycle, read and write limits. A stake-backed genesis supplies `stake.trust` and `stake.observations` from CKB guardians, a signing window, and snapshot limits. The `development_validators` and `development_cells` fields are for fixture networks. A bridged spec pins the CKB genesis and anchor, the vault Type IDs and initial OutPoints, wrapped code, payout Lock templates, and the bridge guardian set.

Compute `fern genesis-hash --genesis fern.json`. Set CometBFT `app_state.fern_genesis` to this hex value, set the same chain ID and genesis time, and use the exact Fern genesis committee. Start Fern in a private home and set CometBFT's ABCI address to the absolute socket path.

```sh
target/release/fern run --home /path/to/fern-home --genesis fern.json --workers 2
target/fern-tools/cometbft start --home /path/to/comet-home \
  --proxy_app unix:///path/to/fern-home/abci.sock
```

Ordinary replicas prune execution bodies and recent Header indices after `--history-window`. The window must cover Header Deps and the stake signing window. `--archive` preserves application bodies and Headers as they arrive. Snapshots export canonical state records and a recent Header suffix. State-sync consumers authenticate the AppHash through CometBFT's light client and rebuild the two state roots before installing. Configure `[statesync]` on a fresh observer with two reachable RPC servers, a trusted signed block height and hash, and reachable P2P peers. `--snapshot-interval` controls asynchronous exports and `--snapshot-bytes` caps the received artifact size.

## Exercise the native CometBFT path

```sh
cargo test --locked -p fern-ledger measure_canonical_payment_and_combined_output_bytes -- --nocapture
cargo test --locked -p ckb-script --features vm-reuse --test fern_native -- --nocapture
FERN_TEST_COMET=target/fern-tools/cometbft \
  cargo test --locked --test native_store four_node_native_finality_restart_and_app_hash \
  -- --ignored --nocapture
```

The four-node test prints its evidence directory. It generates a fresh network, starts four validators, exercises signed payments and a two-Type-Script transfer, restarts a replica and joins a fifth node using remote snapshots. It leaves the test configuration, logs, headers and commit certificates in the evidence directory.

```sh
FERN_NATIVE_EVIDENCE=/path/to/printed/evidence \
  go -C tools/proof test -mod=readonly -run TestNativeCommitSignatures -v .
```

The benchmark uses the same four-node harness. Set `FERN_TEST_LOADGEN`, `FERN_TEST_LOAD_RATE`, `FERN_TEST_LOAD_COUNT` and `FERN_TEST_LOAD_KIND`. Workloads are `payment`, `single`, `double`, `multi` and `settlement`. Set `FERN_TEST_EVIDENCE_ROOT=target/fern-runs` to keep evidence on the workspace filesystem. Use `--release` for throughput characterization.

## Wallet and proofs

`fern wallet build --request request.json` builds a transaction from canonical Cell bytes, inputs, dependencies and any proof data. `fern wallet sign` stores a 65-byte secp256k1 seal at the indicated Lock Group number. The user supplies the intended complete Lock Script and the native genesis. `fern ckb-wallet` signs CKB L1 transactions for test deployment and custody workflows.

CometBFT serves ABCI queries at `/fern/header`, `/fern/cell` and `/fern/execution`. A Live Cell query uses the packed OutPoint. An execution query uses a little-endian `u32` transaction index and a block height. Use `prove=true` for the Patricia path or the matching transaction and receipt Merkle paths. To authenticate header height H, verify the CometBFT commit at H+1 against the native AppHash. Supply signed handoff certificates when validator sets have changed.

## Exercise the native bridge

```sh
contracts_dir=contracts/target/riscv64imac-unknown-none-elf/release
python3 tools/native_bridge_localnet.py \
  --binary target/release/fern --l1 /path/to/ckb --comet target/fern-tools/cometbft \
  --vault "$contracts_dir/vault-type" --deposit-lock "$contracts_dir/deposit-lock" \
  --native-lock "$contracts_dir/fern-lock" --wrapped "$contracts_dir/bridge-udt" \
  --proof-verifier target/fern-tools/fern-proof --output target/fern-runs/native-bridge
```

Add `--xudt /path/to/xudt_rce --xudt-gate "$contracts_dir/xudt-gate"` to deploy a separate xUDT vault and execute both asset round trips. The runner uses isolated CKB and CometBFT nodes and disposable key files. At mint, guardians audit the actual confirmed consolidation and sign the exact native output bytes. To reconcile a payout, they authenticate the Fern execution receipt and the CKB transaction that matches its recipient, quantity and vault nonce.

## Exercise the CKB Stake Registry

```sh
python3 tools/stake_localnet.py \
  --binary target/release/fern --l1 /path/to/ckb \
  --stake-registry "$contracts_dir/stake-registry" --output target/fern-runs/ckb-stake
python3 tools/native_stake_localnet.py \
  --binary target/release/fern --l1 /path/to/ckb --comet target/fern-tools/cometbft \
  --stake-registry "$contracts_dir/stake-registry" \
  --proof-verifier target/fern-tools/fern-proof --output target/fern-runs/native-stake
```

The first runner checks the L1 contract's registration, increase, freeze, retirement, penalty, withdrawal and guardian rotation. The second uses actual L1 observations to initialize a CometBFT committee, add and remove a validator at H+2, verify both signed handoffs, and unbond the removed validator. Use the emitted evidence directories to inspect transaction proofs and exact block heights.

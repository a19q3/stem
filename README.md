# Fern

The node pairs a CometBFT 0.38.26 process with the Rust `fern` ABCI application. Each replica keeps an independent RocksDB database and runs CKB transaction resolution, time locks, capacity checks and CKB-VM. Ordinary transactions are standard Molecule-encoded CKB transactions.

The deployment uses a weighted Ed25519 committee with optional Cell-authorized handoffs. CometBFT authenticates and gossips blocks and votes, provides deterministic ordering with more than two-thirds voting power, and retains a validator's signing state across restart. The application checks proposals before voting and commits the live-cell changes, CKB headers and transaction archive in a single synchronous RocksDB batch. Block sync replays the same checks when joining or catching up. This component is isolated from the L1 runtime.

The CKB header chain uses CometBFT height and time (milliseconds) and a fixed epoch length from genesis. Header deps must reference committed sidechain headers. There is no Cellbase, proposer window, uncle reward or Nervos DAO. A positive fee is consumed as a capacity reduction. Bridged chains mint transferable capacity through attested L1 consolidation and burn it for a certified withdrawal. Public fixture allocations are for development. Compact-target fields are placeholders for encoding CKB headers in their existing format.

The app hash commits to ordered Cell transitions from genesis and changes on blocks with transactions. Empty blocks advance the header clock, and optional empty checkpoints commit an authenticated snapshot for fast state sync. The offline verifier checks both the transaction block's weighted certificate and the successor's authenticated ABCI execution results. Genesis can enable authenticated Live Cell membership and compact transaction checkpoints. Format 3 keeps a permanent index for ordinary transaction hashes. Format 4 uses Live Cell inputs to prevent ordinary transaction replay and preserves CKB headers, Live Cells, and full genesis, bridge and governance transactions. Archive replicas can preserve ordinary transaction bodies. [Checkpoint proof operations](OPERATIONS.md#verify-a-live-cell-at-a-checkpoint)

## Build and run a local committee

Build from this checkout with the pinned Rust toolchain, a C/C++ compiler, libclang, OpenSSL development headers, Go and Python 3.11 or newer. Cargo fetches CKB dependencies at the commit pinned in [Cargo.toml](Cargo.toml).

```sh
cargo build --locked --release -p fern
mkdir -p target/fern-tools target/fern-runs
GOBIN="$PWD/target/fern-tools" go install github.com/cometbft/cometbft/cmd/cometbft@v0.38.26
go -C tools/proof build -mod=readonly -o ../../target/fern-tools/fern-proof .
go build -o target/fern-tools/fern-loadgen tools/loadgen/main.go

python3 tools/localnet.py \
  --binary target/release/fern \
  --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --loadgen target/fern-tools/fern-loadgen \
  --output target/fern-runs/payments \
  --replicas 4 --rate 128 --seconds 15
```

The workspace's full-LTO build uses `cargo build --locked --profile prod -p fern`. Use `target/prod/fern` as the application binary. The matched build comparison records binary size, runtime performance, and execution proofs. [Build profile results](BENCHMARKS.md#cargo-build-profile-comparison)

Set `--outage-at 3 --outage-seconds 5` to kill and restart the last replica with its original signing and application state. The harness uses a loopback network, synthetic genesis capacity and public fixture spending keys. It creates private CometBFT validator keys under the run directory and stops its processes on exit. A run keeps its arguments, commit and binary hashes, individual submission and inclusion timestamps, resource samples, application metrics and a verifiable transaction receipt. It measures latency from submission to polling visibility.

For long CKB payment runs, pass `--prefund-payments` with the Go `--loadgen`. The replicas execute signed funding splits before the measurement window and verify every split on every replica. The harness records this setup in `prefunding.json` and supports up to 262,144 subsequent payments. The application genesis contains the development funding inputs and system Cells.

Use `tools/network_bench.py` to measure an existing test network, or `tools/vm_localnet.py` to exercise the same runner with four KVM guests. The runner supports private HTTP(S) endpoints, verifies the expected genesis on every replica and exports an independently verifiable execution receipt. [Network benchmark operations](OPERATIONS.md#benchmark-an-existing-network)

A manually provisioned node starts the application with `fern run --home PATH --genesis APP_STATE.json`. The home is a private directory and the app listens on `PATH/abci.sock`. Start CometBFT with `--proxy_app unix:///absolute/PATH/abci.sock` and the agreed genesis file containing the same application state under `app_state`. An operator must preserve CometBFT's validator key and latest signing state together. A full replica uses a key absent from the committee. Keep the unrestricted CometBFT RPC on a protected listener.

## RPC and transaction format

Submit a packed CKB `Transaction` with CometBFT's `broadcast_tx_sync` RPC. Check application code zero for admission and query `tx` for execution using the SHA-256 ID returned by CometBFT. The first 32 bytes of receipt data contain the CKB raw transaction hash, which SDKs use to form OutPoints. On chains configured with authenticated withdrawals, a burn receipt appends its first withdrawal nonce as an eight-byte little-endian integer. Additional burns in the same transaction receive consecutive nonces in output order. Wallets need the sidechain's own system script dep outpoints. This interface uses a CometBFT RPC transport and CKB's transaction and VM formats.

Use `abci_query` with a quoted path, a hex-encoded key and `prove=false`. The `/tip` and `/metrics` routes return JSON. The `/live_cell` route takes the 36-byte Molecule OutPoint and returns the live output, data and creation block. The `/header` route takes a CKB header hash and returns the packed header. The `/transaction` route takes a CKB raw transaction hash and returns the 32-byte creation header hash followed by the witnessed packed transaction.

For protocol details see the [CometBFT source](https://github.com/cometbft/cometbft/tree/v0.38.26) and [local CKB chain verification](https://github.com/nervosnetwork/ckb/blob/119598b0a1146a569068ac80bdba7eb5f6292fd3/verification/src/transaction_verifier.rs).

The node, proof verifier, load driver and systemd units use the Fern name. The established genesis, checkpoint, storage and bridge hash domains are part of the protocol and continue to identify existing Fern state and proofs.

The first standalone tree was imported from CKB workspace commit `77624d17b95ffa492b8a4ac8bc5f515a9b96135d`. The [vendored CKB script crate](vendor/ckb-script/README.md) carries the VM allocation reuse used by Fern. The recorded benchmarks and evidence preserve their original application commits and observations.

The [operations guide](OPERATIONS.md) describes the governance Cell, verifier handoffs, backup and recovery, custody roles, and the local bridge round trip.

The bridge supports native CKB and one optional, genesis-pinned plain xUDT asset. The same wallet format submits both. The [xUDT custody procedure](OPERATIONS.md#xudt-custody) gives the supported script profile and the local round-trip command.

See [benchmark results](BENCHMARKS.md) for the release sweep, L1 baseline and raw observations.

## Using a CKB wallet or SDK

Start a sidechain replica with `run --rpc-index` to serve CKB JSON-RPC to wallet software. Run the gateway against that replica with the same application genesis.

```sh
fern rpc --genesis /etc/fern/app-state.json \
  --comet-rpc http://127.0.0.1:26657/ --listen 127.0.0.1:8114
```

The gateway implements `send_transaction`, `get_transaction`, `get_live_cell`, `get_cells`, `get_cells_capacity`, `get_indexer_tip`, and the standard tip, block and header lookups. The wallet uses sidechain CellDep outpoints and an explicit fee rate. Transaction and Cell queries reflect committed state. `get_cells` supports lock and type scripts, exact and prefix search, filters, ascending and descending cursors, and optional Cell data. Block lookups use the replica's retained CometBFT block bodies and check their transaction and witness root against the CKB header.

The integration test uses CCC's stock CKB JSON-RPC client to page Cells by lock, select inputs, compute change, sign, submit, wait for BFT execution and read the confirmed block.

```sh
npm install --prefix target/fern-tools/ccc --save-exact @ckb-ccc/core@1.24.0
python3 tools/wallet_rpc_localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --sdk-prefix target/fern-tools/ccc --output target/fern-runs/wallet-rpc
```

The JSON-RPC wallet subset excludes L1 pool inspection, automatic fee-rate estimation, historical transaction search by script, DAO and mining. Use the [operations guide](OPERATIONS.md#serve-ckb-wallet-rpc) for replication, query limits and history behavior.

Build the raw transaction with the SDK's ordinary `CellInput`, `CellOutput`, `CellDep`, and witness builders. Use outpoints from the sidechain and the sidechain's deployed system dep group. For a one-input payment, save the usual CKB transaction JSON and sign its lock group with the application CLI.

```sh
# groups.json names one input and a private file containing a 32-byte key.
fern wallet sign --transaction payment.json --groups groups.json > signed.json
tx=$(jq -r .molecule signed.json)
curl -fsS --get --data-urlencode "tx=$tx" http://127.0.0.1:26657/broadcast_tx_sync
```

For example, the standard one-input group is `[{"inputs":[0],"keys":["/private/wallet.key"]}]`. The signing utility also supports legacy CKB multisig witness prefixes. SDK implementations can produce the same Molecule bytes and submit them directly. The bridge and governance scenarios build and submit standard signed CKB JSON transactions through this interface.

### Replay the Spore V2 SDK test

The scenario deploys the published Spore V2 `0.2.2-beta.1` ELF into a sidechain code Cell and preserves its `data1` Script identity. The stock CCC client uses the CKB RPC gateway, and the Spore SDK finds assets by Type Script. Four validators finalize mint, transfer and melt with the standard CoBuild witnesses and secp256k1 signatures. The VM rejects a transfer that changes content. The test uses synthetic development capacity and covers the unclustered Spore V2 lifecycle. See the [published contract versions](https://github.com/sporeprotocol/spore-contract/blob/master/docs/VERSIONS.md) and [CCC SDK](https://github.com/ckb-devrel/ccc).

Export `outputs_data[0]` from the CKB L1 transaction `0x96b198fb5ddbd1eed57ed667068f1f1e55d07907b4c0dbd38675a69ea1b69824` as raw bytes. The runner checks the published code hash before starting. Install the pinned SDK versions into a separate directory and point the runner at the ELF.

```sh
npm install --prefix target/fern-tools/ccc --save-exact \
  @ckb-ccc/core@1.24.0 @ckb-ccc/spore@1.6.17
python3 tools/spore_localnet.py \
  --binary target/release/fern \
  --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --spore /path/to/spore-v2.elf --sdk-prefix target/fern-tools/ccc \
  --output target/fern-runs/spore-v2
```

### Replay the CellScript Lock test

The test builds the [2027 equality Lock](https://github.com/CellScript-Labs/CellScript/blob/1250e2c6a4f8db3f7656a7292c624b56618b2d36/examples/lock-script-2027/README.md) using `cellc 0.33.0-alpha.2`. It verifies the ELF's source binding, deploys it as a `data2` Lock and uses `cellc entry-witness` to build the Molecule witness payload. The RPC rejects an incorrect witness at the contract assertion. A second submission changes only the witness and executes in 6,952 cycles on all four validators. The fixture compares public bytes to test the witness ABI and CKB-VM syscall behavior. Use it with development capacity.

With the matching compiler and CellScript checkout, export the pinned package so the test builds in a separate directory.

```sh
mkdir -p target/fern-tools/cellscript-lock
git -C ../CellScript archive 1250e2c6a4f8db3f7656a7292c624b56618b2d36 examples/lock-script-2027 \
  | tar -x -C target/fern-tools/cellscript-lock
python3 tools/cellscript_localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --cellc ../CellScript/target/debug/cellc \
  --package target/fern-tools/cellscript-lock/examples/lock-script-2027 \
  --output target/fern-runs/cellscript-lock
```

## Consensus implementation

The consensus integration uses CometBFT's weighted vote, durable signer and catch-up paths. The local Tondi comparison examined the weighted KarmaDAG liveness diagnostics and native Jolteon implementation. The reported unresolved KarmaDAG liveness case and Jolteon's integration with Tondi-specific ordering, payload and checkpoint services favored the existing CometBFT application boundary.

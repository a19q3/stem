# Deploy and operate Fern

Each host runs the Rust application and CometBFT 0.38.26. Copy the same reviewed CometBFT genesis to every host and keep an independent application home, CometBFT home, node identity and validator signing key per host. Supply an absolute Unix socket path to CometBFT. The application home requires mode 0700. Enable P2P traffic on the configured validator and full-node peers. Keep RPC private or put a restricted gateway in front of the public transaction and query endpoints.

```sh
fern run --home /srv/fern/app --genesis /etc/fern/app-state.json --workers 2
cometbft start --home /srv/fern/comet \
  --proxy_app unix:///srv/fern/app/abci.sock \
  --rpc.laddr tcp://127.0.0.1:26657 \
  --consensus.create_empty_blocks=false --consensus.create_empty_blocks_interval=10s
```

Start the application before consensus. A full replica has a local CometBFT key outside the current committee and follows the same block replay path. By default, replicas retain the CKB transactions they execute, all CKB headers and consensus history. Prometheus is available from CometBFT. The application reports transaction admissions, finalization, script checks, cache hits and synchronous write duration through `abci_query("/metrics")`.

`--workers` controls parallel script execution for proposals. Admission uses the same worker count by default. Set `--admission-workers` to tune admission independently. With the systemd template, set `ADMISSION_ARGS=--admission-workers=3` to select three admission workers for that replica. Measure with the intended host CPU budget and committee size.

`--abci-response-queue` sets the depth of each connection's ordered response queue from 1 to 128. The default is 128. The startup log reports the active depth. In a systemd environment file, `ADMISSION_ARGS="--admission-workers=3 --abci-response-queue=32"` sets both limits. The local process and KVM benchmark runners accept the same queue option.

In `/metrics`, `abci_flushes`, `abci_flush_wait_micros` and `max_abci_flush_wait_micros` count Flush responses and time their wait from request decode to writer service. The ordered writer may wait for earlier CheckTx responses while later checks queue on the same connection. `commit_wait_micros` and `max_commit_wait_micros` measure acquisition of the Commit state lock. `sync_micros` measures the synchronous storage write separately. These process counters restart when the application restarts.

## Provision a bridged test network

Each host initializes its own CometBFT home and supplies its public Ed25519 validator key and voting power for the shared genesis. Set a unique `chain_id`, an agreed `timestamp_ms`, the epoch length, the validator list and the L1 bridge identities in `bridge-spec.json`. Start with empty `allocations`. The bridge configuration uses the L1 genesis hash, the unique vault type hash, the native deposit lock, independent guardian public keys and the confirmation depth. Add the exact xUDT profile only when deploying a version-2 vault.

```sh
fern dev-fixture --spec bridge-spec.json --payments 0 > bootstrap.json
jq '.genesis' bootstrap.json > app-state.json
jq '{deposit_dep_group, xudt_dep}' bootstrap.json > system-deps.json
```

For an xUDT vault, add `--xudt /path/to/xudt_rce`. For on-chain committee management, add `--governance-lock /path/to/governance-lock.json`. To allow deposit guardians to rotate, add `--guardian-governance-lock /path/to/guardian-owner-lock.json`. The zero-payment mode generates the required native system code Cells, the pinned xUDT code if configured, and the optional governance Cells. Spendable capacity comes from verified deposits. Install the full resulting `app-state.json` under the CometBFT genesis `app_state` field. Match the chain ID, timestamp and validator weights in both configurations. On initial startup the application checks that the configurations agree.

## Supervise a replica with systemd

The Linux user service templates run under a dedicated unprivileged operating account. They start CometBFT after the application database opens and the ABCI socket is ready. If the app crashes, systemd restarts the pair against the same data directories and validator signing state. Use systemd 249 or newer. [systemd unit dependencies](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#Upholds=)

Place the three unit templates under the service account's systemd user unit directory. Under a private replica directory, copy `node.env.example` to `node.env` and set the absolute binary, database, genesis and socket paths. Initialize the CometBFT home, copy the agreed genesis into `config/genesis.json`, and configure the P2P peers, private RPC listener and empty-block interval in `config/config.toml`. Keep the ABCI socket path short enough for Unix sockets. Use the same configuration to start a nonvoting full replica, with a key outside the committee. Leave `RETAIN_CONSENSUS_BLOCKS=0` on archival replicas.

```sh
unit=$(systemd-escape --path --template=fern@.target /srv/fern)
systemctl --user daemon-reload
systemctl --user enable --now "$unit"
```

Configure a persistent user manager for the service account when unattended startup is required. Before a coordinated backup, stop the target and both service instances in one `systemctl --user stop` call and wait for it to return. CometBFT shuts down before the application. Use `journalctl --user` with the exact application and CometBFT service instance names for diagnostics. The `tools/systemd_localnet.py` scenario validates four locally managed replicas, kills the one with the wallet index enabled, and checks committed receipts and indexed live Cells after automatic recovery and a coordinated stop and start.

## Benchmark an existing network

Prepare a dedicated test committee, a trusted CometBFT genesis including its CKB `app_state`, and a workload JSON with a `transactions` array of signed packed CKB transactions encoded as hex strings. Fund the workload Cells on the test network. Generated CKB payment fixtures with `funding_transactions` can use `--prefund-payments`, which confirms those splits on every listed replica before measuring. Each run needs fresh inputs.

Provide a topology JSON with a `placement` of `same-host`, `virtualized` or `independent-hosts`, and a `replicas` array of four to 32 entries. Each entry has a unique `label`, its physical `host` label, and an `rpc` URL. An optional `metrics` URL supplies the CometBFT Prometheus endpoint, and `submit: false` makes a replica observation-only. Keep both endpoints on private links or SSH forwards and use HTTPS where the benchmark client connects over an untrusted transport. Host placement is reported from this manifest. The driver checks distinct node identities, the expected sidechain genesis and agreement at a common retained block height before submission.

```sh
python3 tools/network_bench.py \
  --binary target/release/fern --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof --genesis /trusted/genesis.json \
  --topology topology.json --workload signed-workload.json --rate 300 \
  --output target/fern-runs/network-benchmark
```

The driver records client-side timings, per-replica execution observations, app metrics and logical storage. Samples from the CometBFT metrics endpoint include P2P traffic, mempool entries and bytes, rechecks and any counters reported for failed, rejected or evicted transactions. The driver compares observed transaction results and writes a weighted execution proof and its offline verification result. For a rotated committee, provide `--handoffs` with the ordered authenticated handoffs. Retain consensus blocks and execution results over the observation window, including the proof's successor block.

For per-process CPU, RSS, write bytes and allocated directories, run the sampler as the replica's service account on each Linux host. Use the topology label and the two current service PIDs. Keep the samples alongside the run data and align them with `start_unix_ns` in `remote-observations.json`. Synchronize host clocks for that comparison. Samples remain scoped to the original processes, and a stopped or reused PID is recorded as unavailable.

```sh
python3 tools/replica_sample.py --label "$replica" \
  --app-pid "$app_pid" --comet-pid "$comet_pid" \
  --app-home /srv/fern/app --comet-home /srv/fern/comet \
  --seconds 120 > "$replica-resources.jsonl"
```

The KVM harness uses a readable Linux kernel matching the host's modules with built-in virtio PCI, virtio block, ext4, and devtmpfs. It requires QEMU/KVM, e2fsprogs, kmod and iproute2. The guests use separate kernels, keys and ext4 images on one physical host. P2P travels on a guest-only network and RPC forwarding binds to host loopback. Each run records its guest configuration, peer connections, console logs, and per-VM host process and disk samples. The run directory contains private test keys and disk images.

With `sch_netem` available, `--p2p-delay-ms`, `--p2p-loss-percent`, `--p2p-duplicate-percent` and `--p2p-reorder-percent` shape packets on guest P2P egress. Reordering requires a positive delay. The RPC interface uses a separate virtual NIC. `--p2p-netem-seed` requires kernel and iproute2 seed support and adds the zero-based guest index to the base seed. The console records the active traffic-control settings and five-second queue counter samples when shaping is enabled. `--proposal-timeout-ms` overrides the initial CometBFT proposal timeout from the harness default of one second and records the selected value in the topology.

## Backup and restore

Stop CometBFT before stopping the application for a coordinated backup. Keep the genesis and both databases together. Keep the current CometBFT signing-state file paired with the corresponding validator key in protected storage. Restoring an older signing state can make a validator sign twice at the same height. Recover stale backups with a fresh replica identity and replay from known peers. Authorize a replacement key through the governance Cell when the replacement is caught up. Test upgrades on a replica first. Storage schema v2 rebuilds the Cell origin index from retained headers before advancing the database version marker. It resumes safely after an interrupted migration and keeps the certified archive format stable. Keep a pre-upgrade backup for binary rollback. Schedule application rule changes with the committee so deterministic execution remains consistent during activation.

## Roll a compatible application release

Roll one replica at a time when the releases can execute the same consensus rules and read the same persisted state. Exercise the candidate first on a full replica. Coordinate the change with the other operators so the observed committee and the planned offline replicas remain current during the restart.

Use a recent signed canary transaction and select its successor height from the latest common history seen by the operators. Export the execution proof from a retained provider using the trusted genesis. When the committee has rotated, give the exporter its ordered authenticated handoffs. The exported bundle carries those handoffs into the offline check.

```sh
fern-proof -genesis /trusted/fern/genesis.json -rpc "$replica_rpc" \
  -height "$canary_height" -ckb-tx "$canary_ckb_tx" > canary-proof.json
fern-proof -genesis /trusted/fern/genesis.json -proof canary-proof.json \
  -height "$successor_height" -offline "$validator_address"
```

The verifier checks the execution receipt and counts the authenticated block votes remaining in its successor certificate after excluding the selected addresses. `-offline` accepts comma-separated CometBFT validator addresses. A pending change to the next committee or remaining signed power at or below two thirds causes the check to fail. Include any other planned offline validators. This measures participation at the selected height. Check current replica health and wait for any in-progress committee handoff before using that evidence to stop a service. With weights 2, 2, 1 and 1, stopping a weight-2 validator leaves four votes and requires an authorized committee handoff or a coordinated interruption.

Stop the pair in CometBFT-then-application order. Change the application executable and start against the same data directories and latest signer state. Verify a payment finalized while the replica was down after it catches up. Wait for the restarted validator to participate in a new commit before updating another node. For a compatible binary rollback, keep the latest state and switch the executable using the same procedure. Schedule any rule change that alters deterministic execution for a separate committee-coordinated activation.

Run the local upgrade scenario with the two binaries to verify the pair against the format-4 checkpoint and compact-history settings. It verifies signed payments while each validator is offline, then checks catch-up, signing and a binary rollback using the original data directories. The default voting powers allow each individual restart. Pass `--voting-powers 2 2 1 1` to exercise the refusal at exactly two thirds.

```sh
python3 tools/rolling_upgrade_localnet.py \
  --binary /path/to/old/fern --new-binary /path/to/new/fern \
  --comet target/fern-tools/cometbft --proof-verifier target/fern-tools/fern-proof \
  --output target/fern-runs/rolling-upgrade
```

Run `governance_localnet.py --check-upgrade-quorum` with the built proof verifier to exercise the refusal while a new committee is scheduled for the next height. [Signed upgrade and handoff evidence](evidence/2026-11-25-rolling-upgrade.json.xz)

## Serve CKB wallet RPC

Enable `--rpc-index` on the application process serving wallet queries. With the systemd template, set `RPC_INDEX_ARGS=--rpc-index` in that replica's `node.env`. At startup or checkpoint restore, the replica can rebuild the index in a separate RocksDB column family from committed headers and live Cells. An indexed replica updates that column family atomically with the canonical ledger batch. The column family is derived and excluded from certified state archives. Keep a pre-upgrade database backup when enabling the index because older binaries cannot open a database with this column family.

Run `fern rpc` with the same application genesis and the indexed replica's local CometBFT RPC. It validates the backend genesis and binds to a loopback address. Keep the CometBFT RPC private and publish wallet RPC through the site's application proxy when remote clients need access. The gateway bounds request size, batch length and concurrency. The Cell index limits each page to 1,000 Cells and bounds the scan work. An over-budget query returns an error. A narrower script or filter can reduce the lookup's scope. Cursors resume the indexed key order against current live state, so pages reflect commits that occur between requests.

Use an archival replica for historical block bodies, cycle lookups, and complete transaction history. A compact replica can return committed status with a null transaction body. On format 4, a compact replica returns `unknown` for an ordinary transaction when none of its outputs are live and it falls outside the local status window. Use `passthrough` for the CKB outputs-policy argument and configure each SDK with the sidechain's code dep outpoints.

## Checkpoints and pruning

Set `checkpoint_interval` in application genesis to enable certified state sync. Each checkpoint height uses an empty block and authenticates the preceding sorted state archive together with the new CKB header. The verifier streams the archive in bounded chunks and switches databases only after checking the complete digest against the light client's verified application hash. Formats 1 and 2 transfer the full CKB transaction archive. The newest two local snapshots are advertised, and snapshot installation supports archives up to 8 GiB.

Prepare a joining replica with CometBFT `statesync.enable`, two independent RPC providers, and a verified trusted height and block hash. The local test `tools/snapshot_localnet.py` exercises state sync, archival lookup, subsequent execution and crash recovery. Back up the application home together with the `active-store` pointer when it exists.

Use `--serial-replay` in `tools/localnet.py` to verify a fresh single-worker replay after a benchmark. The run requires retained CometBFT blocks and ABCI execution results from height one. It compares the replayed blocks and app hashes against the validators and checks any checkpoint at the comparison height or earlier in the same checkpoint interval. [Replay benchmark results](BENCHMARKS.md#serial-replay-and-checkpoint-equivalence)

Peers advertise a checkpoint at height H after two successor blocks have committed. CometBFT state sync reads H+1 for the application hash and H+2 for the next validator set. [CometBFT state provider](https://github.com/cometbft/cometbft/blob/v0.38.26/statesync/stateprovider.go)

Use `--retain-consensus-blocks` on compact replicas with checkpoints. It preserves at least two checkpoint intervals and never prunes past the oldest advertised snapshot. CometBFT retains the evidence-critical headers it needs. Keep archive peers for old CometBFT blocks, historical light-client verification and transaction inclusion proofs. ABCI queries use `/storage_stats` to scan logical application storage by live cells, transaction archive, headers and withdrawal records.

In the compact replica's CometBFT `config/config.toml`, set `[tx_index]` `indexer = "null"`. CometBFT 0.38.26 prunes block bodies and state but its default `kv` index retains separate transaction copies. Keep `[storage]` `discard_abci_responses = false` for authenticated execution receipts. The CKB wallet gateway and proof export by block height work with `indexer = "null"`. Keep `indexer = "kv"` on archive providers that serve CometBFT `/tx` or `/tx_search`. To reclaim the old index when converting a replica that used the default LevelDB backend, stop the node pair, back up the data, and move `tx_index.db` out of the configured CometBFT data directory before restarting with `indexer = "null"`. [CometBFT state pruning](https://github.com/cometbft/cometbft/blob/v0.38.26/state/execution.go), [CometBFT indexer configuration](https://github.com/cometbft/cometbft/blob/v0.38.26/config/config.go)

Leave the consensus WAL under CometBFT's built-in rotation. Version 0.38.26 checks file sizes every five seconds, rotates a head of at least 10 MiB and trims old segments against its 1 GiB target. Allow disk space for the active head and writes between checks. Preserve the WAL and validator signing state through ordinary restarts. [CometBFT WAL](https://github.com/cometbft/cometbft/blob/v0.38.26/consensus/wal.go), [WAL rotation](https://github.com/cometbft/cometbft/blob/v0.38.26/libs/autofile/group.go)

### Compact transaction checkpoints

Set `checkpoint_compact_transactions: true` alongside `checkpoint_cell_proofs: true` when creating genesis to select format 3. Its canonical archive holds Live Cells and their data, all CKB headers, ordinary transaction hashes and their creation header hashes, and full genesis, bridge mint, withdrawal and committee change transactions. Its ordinary replay index grows as transactions commit.

Add `checkpoint_live_cell_replay: true` to select format 4 at genesis. Format 4 removes the ordinary replay index from canonical state. Every ordinary transaction resolves and spends at least one live input. A first replay would require a previously consumed OutPoint to exist again. Recreating that OutPoint requires an earlier replay or a collision in the raw transaction hash. Genesis allocations and bridge mints use separate rules for creating Cells without inputs and keep permanent transaction records. Same-block duplicate and spend checks remain in effect. Disk planning still includes the live set, headers, full bridge and governance history, RocksDB and CometBFT files, and the two advertised snapshots.

Archive replicas retain ordinary transaction bodies they execute in a separate key range. Create a complete archive by replaying from genesis or restoring a complete operator backup. A replica joining through a compact checkpoint can build ordinary transaction history from subsequent execution. `--compact-transaction-history` deletes any local ordinary transaction body range at startup and omits future ordinary bodies. Set `TRANSACTION_HISTORY_ARGS=--compact-transaction-history` for the systemd template. Back up ordinary history before enabling the flag on an existing archive. RocksDB reclaims the deleted range during compaction.

Format-4 compact replicas index ordinary transaction status locally for the latest 128 block heights they execute in compact mode. Fast sync and the first switch from archive mode start with an empty status window. A query can also recover a transaction's height and creation header hash directly from one of its live outputs. In `/storage_stats`, `transaction_replay_index`, `transaction_archive`, `recent_transaction_status` and `recent_transaction_expiry` report separate logical counts and sizes.

The wallet gateway can return the committed height and header hash even when the body is pruned. The application-level `/transaction` query serves available witnessed transaction bytes, including retained bridge and committee transactions. Execution proofs for ordinary historical payments use the retained CometBFT blocks and execution results on an archive provider.

### Verify a Live Cell at a checkpoint

Set `checkpoint_cell_proofs: true` alongside `checkpoint_interval` when creating genesis to use snapshot format 2. Adding `checkpoint_compact_transactions: true` selects format 3. Adding `checkpoint_live_cell_replay: true` selects format 4. These flags are part of the chain identity. Formats 2 through 4 commit a root over the exact Live Cell records in addition to the snapshot digest. Replicas build the root during the ordered archive scan, persist the proof index in a checkpoint sidecar, and rebuild it during verified snapshot restore. The node keeps the newest two published checkpoints.

Send a single `get_live_cell_proof` request with the OutPoint and checkpoint height to a gateway backed by a replica that retains the checkpoint and its successor commit. Save the JSON-RPC `result` object. Supply the expected height and packed Molecule OutPoint from your application. The packed OutPoint is the transaction hash followed by the four-byte little-endian output index. Use a trusted CometBFT genesis and an independently selected checkpoint height when checking freshness.

```sh
fern-proof -genesis /trusted/fern/genesis.json \
  -cell-proof cell-proof.json -height "$expected_height" -out-point "$expected_out_point"
```

The verifier checks both weighted commits, authenticates the manifest through the successor's app hash, validates both Merkle paths and returns the authenticated packed `CellOutput`, data and creation header hash. After committee rotation, include the ordered handoffs in the bundle or supply them as a JSON array with `-handoffs`. Membership describes the exact checkpoint. Later blocks can spend the Cell. A `null` RPC result carries no cryptographic proof of absence. A replica bootstrapped from a snapshot can serve the rebuilt membership data over `/rpc/live_cell_proof`; use a retained CometBFT history provider for the signed headers at the restore height. The ABCI key is the eight-byte big-endian checkpoint height followed by the packed OutPoint. [CometBFT header commitments](https://github.com/cometbft/cometbft/blob/v0.38.26/types/block.go)

Format 2 uses SHA-256 over the domain string `CKB sidechain authenticated database snapshot v2` terminated by a NUL, the eight-byte little-endian height, the 32-byte CKB checkpoint header hash, the eight-byte little-endian archive size, the 32-byte archive digest and the 32-byte Live Cell root, in that order. The root uses CometBFT's RFC 6962 SHA-256 trees. The inner tree commits leaves containing the packed OutPoint followed by the stored record, which holds the 32-byte origin header hash, a four-byte little-endian transaction index, packed `CellOutput` and raw data. Leaves follow the archive's bytewise OutPoint order. Start a new group before adding a record that would exceed 128 records or 1 MiB of archive data. An oversized single record forms its own group. The outer tree commits the ordered group roots as its leaf payloads. [CometBFT Merkle implementation](https://github.com/cometbft/cometbft/blob/v0.38.26/crypto/merkle/tree.go)

Formats 3 and 4 use the same fields and Merkle rules with the respective domain `CKB sidechain authenticated database snapshot v3` or `CKB sidechain authenticated database snapshot v4` followed by a NUL. Their snapshot metadata appends the byte `3` or `4` to the format-2 encoding. Use a verifier with support for the expected format.

```sh
python3 tools/snapshot_localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --output target/fern-runs/checkpoint-cell-proof
```

Add `--checkpoint-compact-transactions` or `--checkpoint-live-cell-replay` to the scenario to exercise format-3 or format-4 state sync. The scenario also restarts an existing replica with local transaction history pruning enabled.

### Export and verify an execution receipt

Use the CKB transaction hash and its committed block height. Wait for the successor block and select a provider that still retains the transaction block and results. For a latest committed transaction, the CKB `get_transaction` response provides its height. The exporter builds a transaction inclusion proof from the CometBFT block, authenticates the block and its successor with the weighted committee, checks the execution result, then writes the bundle to standard output. After a committee change, pass the ordered authenticated handoffs with `-handoffs`. The bundle includes those handoffs for offline verification.

```sh
fern-proof -genesis /trusted/fern/genesis.json \
  -rpc http://127.0.0.1:26657 -height "$height" -ckb-tx "$ckb_tx_hash" > execution-proof.json
fern-proof -genesis /trusted/fern/genesis.json -proof execution-proof.json
```

## Validator changes

An optional genesis governance OutPoint names a one-time capability Cell. Its data starts with `CKB-GOV1`, followed by the sorted 32-byte Ed25519 public keys and their positive 64-bit little-endian voting powers. The `committee-data` command packs that data from JSON. Choose a reviewed threshold lock for the capability. The signed CKB update consumes the current Cell and recreates it as output zero with the same capacity and the next committee in its data. The spend can update the owner lock as well. The capacity remains reserved to the capability. A change included at height H activates at H+2. Add and sync incoming validators before submission and keep the old quorum online through H+1. The `/committee` query shows the most recently authorized set and its activation height.

The execution-proof verifier authenticates a handoff through the old set's signed `next_validators_hash`. Include handoffs in order and keep the original trusted chain configuration. Proofs anchored at genesis assume the required voting-power threshold of each historical committee remains trustworthy. Protect retired keys and retain independently verified checkpoints as part of a long-lived deployment.

The partition scenario exercises the quorum boundary on four local validators. Loopback TCP proxies sever the links between voting weights four and two for six seconds, while both sides continue to admit independent signed payments. The scenario observes committed heights during isolation, reopens the links and verifies a weighted execution proof for each payment after all replicas agree.

```sh
python3 tools/partition_localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --output target/fern-runs/weighted-partition
```

## Native CKB custody

The bridge uses three distinct authorities. The sidechain committee finalizes Cells. Weighted deposit guardians independently read CKB L1 and need more than two thirds of their configured voting power to mint. A separate L1 multisig controls the vault and authorizes releases and custody rotations.

The L1 deposit lock permits consumption only alongside a spend of the canonical vault. The native vault type preserves the unique Type ID, verifies conservation on consolidation, binds a withdrawal to one monotonically increasing slot and enforces the exact native CKB amount and recipient of the signed payout. Vault rotation preserves balance, slot and deposit-lock identity. The custody signers independently verify the sidechain burn and its execution receipt before signing the L1 payout. L1 enforces the multisig and vault rules. Sidechain transaction inclusion and execution are verified by the custodian from the BFT proof.

The bridge binds mint signatures to the L1 genesis, vault, sidechain genesis, deposit owner and amount, plus the canonical consolidation and confirmation blocks. The guardian reads the configured L1 confirmation height, rechecks canonical hashes and persists the highest approved L1 anchor before returning a signature. Each guardian keeps a private journal per operator. Use `bridge check-anchor` before custody approval and treat an unavailable or displaced anchor as a release halt. A reorg deeper than the configured depth can remove backing after mint finality. Custodians must monitor confirmed L1 anchors and suspend new approvals when backing is disputed. Genesis defines the initial guardian set and can enable a separate guardian governance Cell. The L1 vault multisig can rotate through an on-chain transaction.

The coordinator can pass the collected guardian observations to `fern bridge mint --genesis app-state.json --submit-rpc http://127.0.0.1:26657 vote-*.json`. The command verifies the certificate and the replica's genesis, submits the packed transaction, and waits for the archive to show execution. Retrying with another valid set of signatures for the same deposit returns the original mint's block. Use an independently verified execution proof when an external consumer needs evidence beyond the local replica's response.

The vault and deposit contracts have passed the included local scenario and await independent security audit. Their deployment boundary is test networks with test capacity. The vault multisig can authorize asset release under its configured threshold. Loss of that threshold can halt withdrawals and compromise of it can authorize fraudulent payout instructions. Assets from an authorized deposit are already held in the vault until a payout. Bridged genesis reserves capacity for immutable system Cells and the configured governance Cells.

## Rotate deposit guardians

On a bridged chain initialized with `guardian_governance`, the guardian governance Cell's CKB lock authorizes policy changes. Use a reviewed threshold lock with its own operator policy. `bridge guardian-data --members GUARDIANS.json` packs the new set from a JSON array of Ed25519 public keys and positive voting powers. An authorized transaction consumes the current Cell and recreates it at output zero with the same capacity and the new data. It can also rotate the owner lock. Validator and guardian changes use separate Cells and transactions. A change at height H activates for mint validation at H+1.

The `/bridge_guardians` ABCI query returns the active members, the governance OutPoint under `generation`, and the highest L1 confirmation recorded by executed mints under `backing_anchor`. Each `bridge attest` signer on a rotatable chain supplies `--sidechain-rpc` pointing to its own synchronized replica. The signer checks the backing hash against its L1 node and pins it in the journal before returning a signature, including when the journal is new. A fork that displaced that hash halts signing even after an entire guardian set is replaced.

Certificates bind the generation and previously committed L1 backing anchor. More than two thirds of the active voting power must sign the same context. After either value advances, re-attest pending deposits against the synchronized replica. A block can include multiple certificates built against its parent context. A deposit keeps the same L1 address and sidechain mint hash throughout this process. With `--submit-rpc`, `bridge mint` can use the submission replica for policy lookup, and a retry for a previously finalized mint returns the archived result. Use `--sidechain-rpc` for construction without submission. The active policy, backing anchor and signed rotations survive certified compact state sync. Chains provisioned with fixed guardians use the original certificate format.

Run the native bridge scenario with `--rotate-guardians --check-reorg` to exercise a disjoint replacement, a same-key refresh, stale and relabelled certificates, weighted threshold enforcement, state sync and signing with a fresh journal after a dev-chain reorg.

## Replay the verified bridge path

```sh
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --manifest-path contracts/Cargo.toml \
  --release --target riscv64imac-unknown-none-elf

contracts=contracts/target/riscv64imac-unknown-none-elf/release
python3 tools/bridge_localnet.py \
  --binary target/release/fern \
  --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --l1 /path/to/ckb --vault "$contracts/vault-type" \
  --deposit-lock "$contracts/deposit-lock" \
  --output target/fern-runs/bridge-local
```

The harness deploys the RISC-V contracts, funds the vault, runs four sidechain validators and exercises consolidation, attested mint, signed transfer, burn, offline proof verification and vault release. It then moves custody to a disjoint signing group and completes another deposit and withdrawal. The L1 dev node uses paced native blocks with all consensus and VM checks. Rejection cases cover shallow confirmations, insufficient guardian power, a duplicate mint with a different witness, consuming a deposit without the vault, a duplicate payout instruction and signatures from the retired custody group.

## Prepare and verify a withdrawal

Set `authenticate_withdrawals: true` in a new bridged chain's application genesis. The signed execution results then record the first withdrawal nonce in each burn transaction. Use a synchronized L1 node, the trusted CometBFT genesis including `app_state`, and the execution proof with any required committee handoffs. `prepare-payout` finds the burn assigned to the live vault slot and builds an unsigned CKB transaction. The vault and fee input files each contain a CKB OutPoint. The fee input is a plain committed live Cell, and change goes to its existing lock. Supply the fee in shannons and the required L1 CellDeps as a JSON array. For a typed withdrawal, also pass the live xUDT escrow OutPoint with `--escrow` and include the escrow lock and xUDT dependencies.

```sh
fern bridge prepare-payout \
  --comet-genesis /etc/fern/genesis.json \
  --proof-verifier /usr/local/bin/fern-proof --proof withdrawal-proof.json \
  --l1-rpc http://127.0.0.1:8114 \
  --journal /private/custody/guardian-journal --vault vault-outpoint.json \
  --fee-input fee-outpoint.json --fee 1000000 --cell-deps l1-deps.json > release.json
```

Before signing, each custodian checks the exact proposed transaction using its own synchronized L1 node and guardian journals. `check-payout` recomputes the beneficiary and release state and checks that the journal anchors still agree with L1.

```sh
fern bridge check-payout \
  --comet-genesis /etc/fern/genesis.json \
  --proof-verifier /usr/local/bin/fern-proof --proof withdrawal-proof.json \
  --l1-rpc http://127.0.0.1:8114 \
  --journal /private/custody/guardian-journal --vault vault-outpoint.json \
  --transaction release.json
```

Compare the returned `l1_transaction_hash` with the transaction hash produced by the signing system. Recheck a candidate after changing any input, output or dependency. A competing L1 spend can invalidate the live vault outpoint between checking and broadcast. The vault contract checks the consumed outpoint on chain and the custodian can build and recheck a fresh candidate against its successor. The operators choose whose guardian journal copies to check and keep their source and freshness under independent control.

For a chain created with legacy receipts, pass `--sidechain-rpc` to both commands, pointing to the custodian's independently synced replica. The replica supplies the canonical withdrawal index. The verifier still authenticates the burn transaction through the signed receipt. Keep the genesis flag fixed for existing chains because it determines the consensus execution result.

## Rotate L1 vault custody

Prepare two reviewed JSON files for the current and incoming CKB multisigs. Each file has `require_first_n`, `threshold`, and an ordered `pubkey_hashes` array of distinct 20-byte hex values. A standard CKB secp256k1 member can derive this value from `fern wallet key` and share the `blake160` field. The incoming group can demonstrate control by spending a separately funded test Cell with the exact lock that will secure the vault.

Use the same trusted application genesis, reviewed policies, live vault OutPoint, fee Cell and dependencies for both commands. The builder derives the incoming lock from the multisig configuration and leaves the fee change under the fee input lock. The migrated vault carries its Type ID, CKB capacity, asset profile and next withdrawal slot forward. The xUDT escrow stays live at its existing OutPoint during the rotation.

```sh
rotation_args=(
  --genesis /etc/fern/app-state.json --l1-rpc http://127.0.0.1:8114
  --vault vault-outpoint.json --current-multisig current-multisig.json
  --new-multisig incoming-multisig.json --fee-input fee-outpoint.json
  --fee 1000000 --cell-deps l1-deps.json
)
fern bridge prepare-vault-rotation "${rotation_args[@]}" > rotation.json
fern bridge check-vault-rotation "${rotation_args[@]}" --transaction rotation.json
```

Each outgoing custodian checks the exact unsigned transaction using its own L1 node. Compare `l1_transaction_hash` with the signing system's hash, collect the outgoing quorum and fee signatures, and submit the signed transaction. The checker returns the new vault OutPoint and pending withdrawal slot. Wait for the configured L1 confirmation depth and confirm that the reported vault is live before resuming consolidation and payouts under the incoming multisig. A withdrawal proved before rotation can be released against the same slot by the new custodians.

## xUDT custody

A version-2 vault can custody native CKB and one plain xUDT asset. Genesis `bridge.xudt` pins the exact L1 `l1_type` Script and the `gate_code_hash`. The type must use `data1` and default owner flags. Store the same xUDT ELF in an immutable sidechain genesis code Cell. Use an audited release and check the ELF's hash, the full L1 type arguments, and the vault's committed asset profile when provisioning. Vault program identities are content-addressed. A native version-1 vault keeps its existing contract identity, and the asset profile of a version-2 vault is immutable. See the [xUDT RFC](https://github.com/nervosnetwork/rfcs/blob/master/rfcs/0052-extensible-udt/0052-extensible-udt.md).

Use `bridge escrow-cell` to obtain the L1 lock, type and initial data for a zero-unit escrow Cell. Fund its capacity separately and provision it before accepting deposits. Use `bridge deposit-cell --units N --recipient LOCK.json` to generate the exact L1 lock, type and data. The lock arguments commit to the vault, sidechain genesis and packed recipient. The data is an unsigned 16-byte little-endian quantity. Check the entire descriptor before sending funds because a Cell carrying an incorrect asset at the deposit lock may be unspendable.

The L1 gate requires the unique vault as a transaction input. The vault adds all consolidated deposit capacity to its native CKB balance and verifies the corresponding increase in escrow units. An escrow transition preserves the carrier capacity. The guardians attest the canonical consolidation and the sidechain mints a Cell using the verified deposit capacity and units. The wrapped type runs the same xUDT ELF under a derived owner identity. Sidechain consensus requires exact conservation in ordinary transactions and allows supply increases only through attested deposits. Native CKB deposits keep their existing format.

Use `bridge burn-cell --units N --l1-recipient LOCK.json` to construct the typed burn. Its capacity becomes the L1 payout capacity. The `/withdrawal` record returns the exact recipient lock, original L1 type and decimal unit string. Custodians independently verify the executed burn and the L1 anchor before signing. The vault requires the recipient at output zero, bounds release to the next payout slot, and enforces the exact quantities and recipient in the multisig-authorized vault state. Native and typed withdrawals share the nonce sequence. The xUDT gate and version-2 vault await independent audit and have been exercised with local test funds.

```sh
contracts=contracts/target/riscv64imac-unknown-none-elf/release
python3 tools/xudt_bridge_localnet.py \
  --binary target/release/fern \
  --comet target/fern-tools/cometbft \
  --proof-verifier target/fern-tools/fern-proof \
  --l1 /path/to/ckb --vault "$contracts/vault-type" \
  --deposit-lock "$contracts/deposit-lock" --xudt-gate "$contracts/xudt-gate" \
  --xudt /path/to/xudt_rce --output target/fern-runs/xudt-local
```

The fixture uses the deployed xUDT ELF pinned by [Fiber's contract fixtures](https://github.com/nervosnetwork/fiber/blob/4e899912b17ea4859cdb86067e97b2d19ef48f7b/tests/deploy/contracts/README.md). A successful run records both chain transactions, observed payout and escrow balances, the weighted execution proofs and rejection cases. It also proves a pending typed withdrawal before custody rotation and releases it afterward with the incoming signing group.

## Exercise the CKB Stake Registry

Use a local CKB binary, Rust's `riscv64imac-unknown-none-elf` target, and Python with the `cryptography` module. The runner creates a fresh CKB development chain using fixture keys and capacity. It signs L1 transactions with the Fern wallet, deploys the Registry Type ID contract, and reports confirmed transaction and witness proofs.

```sh
cargo build --locked -p fern
RUSTFLAGS='-C passes=lower-atomic -C target-feature=-c' \
  cargo build --locked --manifest-path contracts/Cargo.toml \
  --release --target riscv64imac-unknown-none-elf
python3 tools/stake_localnet.py \
  --binary target/debug/fern --l1 /path/to/ckb \
  --stake-registry contracts/target/riscv64imac-unknown-none-elf/release/stake-registry \
  --output target/fern-runs/stake
```

The runner covers registration, stake increases, guardian-authorized activation and retirement, early withdrawal rejection, CKB-enforced unbonding, voting-evidence verification and slashing, guardian handoff, and withdrawal of the full unslashed bond. It also invokes `fern stake observe` against the running L1 node. Each observation binds the confirmed Registry data, live Stake Cells, prior L1 anchor, and target epoch. `fern stake verify` checks the observations against a separate trust file containing the network identities, prior accepted snapshot, guardian committee and selection policy. The runner writes representative request, observation and trust files under its output directory. Its activation and retirement attestations use development fixtures for Fern committee history.

On the current Fern bridge, the payout path rejects reserved vault-side deposit and xUDT gate code hashes, unsupported hash types, oversized recipient scripts, and outputs below CKB occupied Capacity. A signed typed burn to the vault's own escrow is tested before the valid withdrawal at the same nonce.

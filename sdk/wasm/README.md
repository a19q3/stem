# Stem WASM SDK

The generated ES module runs the native Strand codec, Jolteon verifier
and proof bindings in a browser or Node.js. It includes TypeScript
declarations. Capacity, fee amounts, heights and timestamps are JavaScript
`bigint` values. Binary inputs and outputs use `Uint8Array`.

## Build

From the workspace root, with the toolchain in `rust-toolchain.toml`:

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked --version 0.2.118 wasm-bindgen-cli
sdk/wasm/build.sh /tmp/stem-wasm-package
npm pack /tmp/stem-wasm-package
```

TypeScript consumers can include `ESNext.Disposable` in their compiler library list for the generated handle declarations.

Serve the generated JavaScript and WASM together. The WASM response should
use `application/wasm`. In Node.js, pass the WASM bytes to
`init({ module_or_path: bytes })`.

## Verify and sign

Export a trust bundle from a trusted network specification using
`stem jolteon-trust --genesis stem.json`. Pin its `network_id`,
`genesis_id` and `initial_committee` as one bundle. The application
selects its trust bundle independently of RPC responses used for proofs.

```ts
import init, { StemVerifier, SigningContext, decodeTransaction } from "@stem-network/wasm";

await init();
const verifier = new StemVerifier(networkId, genesisId, initialCommittee);
for (const proof of priorHandoffs) verifier.acceptHandoff(proof);
const header = verifier.verifyFinality(finalityProof, false);
const execution = header.verifyExecution(transactionBytes, receiptBytes, executionProof);

const signing = new SigningContext(header);
for (const { record, proof } of provenInputAndDependencyCells) signing.addLiveCell(record, proof);
const draft = decodeTransaction(unsignedTransaction);
const digest = signing.signingDigest(draft, groupIndex, packedLock);
const signed = draft.withSeal(groupIndex, await externalSigner.signRecoverable(digest));
const submissionBytes = signed.encode();
```

Authenticate earlier Header Deps with the same trust bundle and add them
using `signing.addHeader`. Set all group proof data before collecting seals.
The digest is the native normal Lock digest. The external signer provides the
seal format required by that Lock, such as a compact 65-byte recoverable
secp256k1 signature for the standard Lock.

Use `CellBuilder` and `TransactionBuilder` to create Cells, add the
sorted Type Script set, attach Cell or Header Deps and group Actions,
and encode an ordinary transaction. `verifyLiveCell` returns `false`
for an authenticated absence when passed an empty record. Invalid proofs
throw, including a non-membership proof paired with a supplied record.
`verifyIntercell` uses the same presence convention. `verifyVaultRevenue`
proves both the allocation and fee total against one SystemRoot.
`verifySystemRecord` accepts the protocol key for additional accounting
and stake records.

Use `LineageRef.deriveId(networkId, anchorOutPoint, outputIndex)` for registration.
`new LineageRef(id)` selects `Latest` and `new LineageRef(id, 42n)` selects
version 42. Pass `reference.toBytes()` to `/stem/lineage`. Authenticate the
returned proof against its finalized Header.

```ts
const head = header.verifyLineage(reference, markerCodeHash, record, proof, cell, cellProof);
if (head) console.log(head.version(), head.outPoint());
```

Supply empty byte arrays for absent `record`, `cell` and `cellProof`.
`Latest` can authenticate absence. An exact reference requires a proof of
that version at a Header where it was live. Keep old proof bundles to read
older versions after the Head advances.

Each verified Header describes its own height. Choose a recent finalized
Header for signing and refresh the signing state when the application's
freshness policy requires it. A client starting from genesis relies on the
historical committee's trust assumptions throughout handoffs. For long
offline periods, obtain a recent trusted checkpoint using the network's
CKB anchoring policy. Workspace documentation describes the CKB
inclusion and confirmation assumptions.

The scope includes standard complete group authorization and proof checks.
Networking and the external signing device are supplied by the application.
See [the live SDK fixture](../../OPERATIONS.md#exercise-the-browser-sdk) for
reproducible browser and Node.js tests.

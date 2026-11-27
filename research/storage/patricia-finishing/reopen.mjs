import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const dir = pathToFileURL(resolve(process.argv[2]) + "/");
const sdk = await import(new URL("stem_wasm.js", dir));
await sdk.default({ module_or_path: await readFile(new URL("stem_wasm_bg.wasm", dir)) });
const bytes = s => Buffer.from(s.replace(/^0x/, ""), "hex");
const archive = process.argv[3] ?? fileURLToPath(new URL("reopen.json.xz", import.meta.url));
const data = JSON.parse(execFileSync("xz", ["-dc", archive]));
const {network_id, genesis_id, initial_committee} = data.trust;
const verifier = new sdk.StemVerifier(bytes(network_id), bytes(genesis_id), bytes(initial_committee));
function checked(proof, height) {
    const header = verifier.verifyFinality(bytes(proof), false);
    assert.equal(header.height, BigInt(height));
    return header;
}
const initial = data.report;
checked(initial.two_chain_proof, initial.proof_verification.height).free();
assert.equal(initial.payments, data.workload.length);
for (const status of initial.resumed) assert.equal(status.committed_transactions, initial.payments);
assert.equal(data.previous.point, data.spent.point);
for (const [entry, present] of [[data.previous, true], [data.spent, false], [data.successor, true]]) {
    const header = checked(entry.finality, entry.header.height);
    assert.equal(header.verifyLiveCell(bytes(entry.point), bytes(entry.cell ?? ""), bytes(entry.proof)), present);
    header.free();
}
const sample = data.execution;
const header = checked(sample.finality, sample.header.height);
assert.deepEqual(bytes(sample.transaction), bytes(sample.signed));
const execution = header.verifyExecution(bytes(sample.transaction), bytes(sample.receipt), bytes(sample.proof));
assert.deepEqual(Buffer.from(execution.transactionId()), bytes(sample.id));
const signed = sdk.decodeTransaction(bytes(sample.transaction));
assert.deepEqual(Buffer.from(signed.input(0)), bytes(data.spent.point));
assert.deepEqual(bytes(data.successor.point), Buffer.concat([bytes(sample.id), Buffer.alloc(4)]));
console.log("Verified the reopened history, three Cell proofs and successor execution at", sample.header.height);

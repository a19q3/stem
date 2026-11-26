import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";

const folder = pathToFileURL(resolve(process.argv[2]) + "/");
const sdk = await import(new URL("stem_wasm.js", folder));
await sdk.default({ module_or_path: await readFile(new URL("stem_wasm_bg.wasm", folder)) });
const bytes = (s) => Buffer.from(s.replace(/^0x/, ""), "hex");
const trust = JSON.parse(await readFile(new URL("committee-trust.json", import.meta.url)));
const evidence = {};
for (const filename of ["histories.json.xz", "two-registrations.json.xz", "cluster-settlement.json.xz",
    "handoff-compact.json.xz", "handoff-cluster.json.xz", "handoff-large.json.xz"]) {
    const archive = new URL(filename, import.meta.url);
    Object.assign(evidence, JSON.parse(execFileSync("xz", ["-dc", fileURLToPath(archive)], {maxBuffer: 32*1024*1024})));
}
for (const [name, root] of Object.entries(trust)) {
    const record = evidence[name].settlement;
    const verifier = new sdk.StemVerifier(bytes(root.network_id), bytes(root.genesis_id), bytes(root.initial_committee));
    const transactions = "imported" in record
        ? { imported: record.imported, burned: record.burned, reimported: record.reimported }
        : { issue: record.issued, lock: record.locked, restore: record.restored, relock: record.relocked,
            retire: record.standard_melt.retired };
    const grants = new Map((record.handoffs ?? []).map((grant, i) => [`handoff-${i+1}`, grant]));
    for (const [label, grant] of grants) transactions[label] = grant.native;
    for (const [operation, tx] of Object.entries(transactions)) {
        const sample = record.execution[operation];
        const header = verifier.verifyFinality(bytes(sample.finality), false);
        assert.equal(header.height, BigInt(tx.height));
        const execution = header.verifyExecution(bytes(sample.transaction), bytes(sample.receipt), bytes(sample.proof));
        assert.equal(Buffer.from(execution.transactionId()).toString("hex"), tx.hash);
        if (grants.has(operation)) {
            const grant = grants.get(operation);
            const decoded = sdk.decodeTransaction(bytes(sample.transaction));
            const source = bytes(grant.message.record).subarray(136 + 100, 136 + 136);
            assert.equal(Array.from({length: decoded.inputCount}, (_, i) => Buffer.from(decoded.input(i)))
                .filter(point => point.equals(source)).length, 1);
            const successor = decoded.output(1);
            assert.deepEqual(Buffer.from(successor.encode()), bytes(grant.successor));
            successor.free();
            decoded.free();
        }
        if (operation === "relock" && record.stale_handoff) {
            const current = record.stale_handoff.unspent_native_source;
            const index = Buffer.alloc(4);
            index.writeUInt32LE(Number(current.index));
            const source = Buffer.concat([bytes(current.tx_hash), index]);
            const decoded = sdk.decodeTransaction(bytes(sample.transaction));
            assert.ok(Array.from({length: decoded.inputCount}, (_, i) => Buffer.from(decoded.input(i)))
                .some(point => point.equals(source)));
            decoded.free();
        }
        console.log(name, operation, tx.height, execution.cycles.toString());
        execution.free();
        header.free();
    }
    verifier.free();
}

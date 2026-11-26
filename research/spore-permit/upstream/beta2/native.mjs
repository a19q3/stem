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
const archive = new URL("histories.json.xz", import.meta.url);
const evidence = JSON.parse(execFileSync("xz", ["-dc", fileURLToPath(archive)], {maxBuffer: 32*1024*1024}));
for (const [name, root] of Object.entries(trust)) {
    const record = evidence[name].settlement;
    const verifier = new sdk.StemVerifier(bytes(root.network_id), bytes(root.genesis_id), bytes(root.initial_committee));
    const transactions = name === "import"
        ? { imported: record.imported, burned: record.burned, reimported: record.reimported }
        : { issue: record.issued, lock: record.locked, restore: record.restored, relock: record.relocked,
            retire: record.standard_melt.retired };
    for (const [operation, tx] of Object.entries(transactions)) {
        const sample = record.execution[operation];
        const header = verifier.verifyFinality(bytes(sample.finality), false);
        assert.equal(header.height, BigInt(tx.height));
        const execution = header.verifyExecution(bytes(sample.transaction), bytes(sample.receipt), bytes(sample.proof));
        assert.equal(Buffer.from(execution.transactionId()).toString("hex"), tx.hash);
        console.log(name, operation, tx.height, execution.cycles.toString());
        execution.free();
        header.free();
    }
    verifier.free();
}

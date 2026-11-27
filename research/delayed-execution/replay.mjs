import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const folder = pathToFileURL(resolve(process.argv[2]) + "/");
const sdk = await import(new URL("stem_wasm.js", folder));
await sdk.default({ module_or_path: await readFile(new URL("stem_wasm_bg.wasm", folder)) });
const archive = process.argv[3] ?? fileURLToPath(new URL("ckb-replay.json.xz", import.meta.url));
const evidence = JSON.parse(execFileSync("xz", ["-dc", archive]));
const bytes = (hex) => Buffer.from(hex.replace(/^0x/, ""), "hex");

function firstHeader(proof) {
    const raw = bytes(proof);
    assert.equal(raw[0], 10);
    let index = 1;
    let length = 0;
    let shift = 0;
    for (let octet; ; shift += 7) {
        octet = raw[index++];
        assert.ok(octet !== undefined && shift <= 21);
        length += (octet & 127) << shift;
        if (!(octet & 128)) break;
    }
    const header = raw.subarray(index, index + length);
    assert.equal(header.length, 208);
    return header;
}

for (const { name, trust, checkpoint_type, checkpoints } of evidence.routes) {
    const verifier = new sdk.StemVerifier(bytes(trust.network), bytes(trust.genesis), bytes(trust.roster));
    if (checkpoints[0].terminal) {
        assert.throws(() => verifier.verifyFinality(bytes(checkpoints.at(-1).proof), false));
    }
    for (const { terminal, proof, header, bundle, publication, signed } of checkpoints) {
        const result = terminal ? verifier.acceptHandoff(bytes(proof)) : verifier.verifyFinality(bytes(proof), false);
        assert.equal(result.height, BigInt(header.height));
        assert.deepEqual(Buffer.from(result.appHash()), bytes(header.app_hash));
        assert.deepEqual(firstHeader(proof), bytes(header.header));
        assert.equal(bundle.height, publication.height);
        assert.equal(publication.transaction, signed.hash);
        const index = signed.json.outputs.findIndex((output) => JSON.stringify(output.type) === JSON.stringify(checkpoint_type));
        assert.ok(index >= 0);
        assert.deepEqual(bytes(signed.json.outputs_data[index]), bytes(bundle.tip));
        assert.deepEqual(bytes(bundle.tip).subarray(64), firstHeader(proof));
        result.free();
        console.log(name, header.height, terminal ? "handoff" : "execution");
    }
    verifier.free();
}

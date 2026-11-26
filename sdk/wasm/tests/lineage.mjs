import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [packageDir, fixtureFile, trustFile] = process.argv.slice(2);
const sdk = await import(pathToFileURL(resolve(packageDir, "stem_wasm.js")));
await sdk.default({ module_or_path: await readFile(resolve(packageDir, "stem_wasm_bg.wasm")) });
const fixture = JSON.parse(await readFile(fixtureFile));
const trust = JSON.parse(await readFile(trustFile));
const bytes = (hex) => Buffer.from(hex.replace(/^0x/, ""), "hex");
const id = bytes(fixture.id);
const network = bytes(trust.network_id);
const anchor = bytes(fixture.registration_anchor);
assert.deepEqual(Buffer.from(sdk.LineageRef.deriveId(network, anchor, 0)), id);
assert.throws(() => sdk.LineageRef.deriveId(network, anchor, 2 ** 32));

const latest = new sdk.LineageRef(id);
const first = new sdk.LineageRef(id, 0n);
const exact = new sdk.LineageRef(id, 4n);
assert.deepEqual(Buffer.from(latest.toBytes()), Buffer.concat([id, Buffer.of(0)]));
assert.deepEqual(Buffer.from(first.toBytes()), Buffer.concat([id, Buffer.of(1)]));
const maximum = (1n << 64n) - 1n;
assert.equal(sdk.LineageRef.fromBytes(new sdk.LineageRef(id, maximum).toBytes()).version(), maximum);
assert.throws(() => new sdk.LineageRef(id, -1n));
assert.throws(() => new sdk.LineageRef(id, maximum + 1n));
assert.throws(() => sdk.LineageRef.fromBytes(Buffer.concat([id, Buffer.of(128, 0)])));

const verifier = new sdk.StemVerifier(network, bytes(trust.genesis_id), bytes(trust.initial_committee));
const historical = verifier.verifyFinality(bytes(fixture.historical.finality), false);
const current = verifier.verifyFinality(bytes(fixture.current.finality), false);
assert.equal(historical.height, BigInt(fixture.historical.height));
assert.equal(current.height, BigInt(fixture.current.height));
const argumentsFor = ({ head }) => [bytes(fixture.marker_code), bytes(head.record),
    bytes(head.proof), bytes(head.cell), bytes(head.cell_proof)];
const previous = argumentsFor(fixture.historical);
const active = argumentsFor(fixture.current);
assert.equal(historical.verifyLineage(first, ...previous).version(), 0n);
assert.equal(current.verifyLineage(latest, ...active).version(), 4n);
assert.equal(current.verifyLineage(exact, ...active).version(), 4n);
assert.throws(() => current.verifyLineage(first, ...active));
assert.throws(() => current.verifyLineage(first, ...previous));
assert.throws(() => historical.verifyLineage(exact, ...active));
console.log(JSON.stringify({ historical_height: String(historical.height), current_height: String(current.height),
    historical_version: "0", current_version: "4", latest_reference_bytes: latest.toBytes().length }));

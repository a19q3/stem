import { readFile } from "node:fs/promises";
import init from "./stem_wasm.js";
import { run } from "./smoke.mjs";

const payload = (file) => readFile(new URL(file, import.meta.url));
const start = performance.now();
await init({ module_or_path: await payload("stem_wasm_bg.wasm") });
const initMs = performance.now() - start;
const fixtures = await Promise.all(["fixture.json", "handoff.json"].map(async (file) => JSON.parse(await payload(file))));
console.log(JSON.stringify({ engine: process.version, init_ms: initMs, ...run(...fixtures) }));

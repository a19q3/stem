import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [sdk, reportPath, trustPath] = process.argv.slice(2);
const { default: init, decodeCell, StemVerifier } = await import(pathToFileURL(resolve(sdk, "stem_wasm.js")));
await init({ module_or_path: await readFile(join(sdk, "stem_wasm_bg.wasm")) });
const readJson = async (path) => JSON.parse(await readFile(path, "utf8"));
const bytes = (raw) => Buffer.from((raw ?? "").replace(/^0x/, ""), "hex");
const trust = await readJson(trustPath);
const verifier = new StemVerifier(bytes(trust.network_id), bytes(trust.genesis_id), bytes(trust.initial_committee));
const { metatype_composed_cell: fixture } = await readJson(reportPath);
const { migration } = fixture;
const reorg = migration.reorg;

function certificate(entry) {
  const header = verifier.verifyFinality(bytes(entry.finality), false);
  assert.equal(header.height, BigInt(entry.height));
  return header;
}

const executions = {};
for (const [name, row] of Object.entries({
  original_export: fixture.execution.export,
  original_partial_return: fixture.execution.redeem,
  migration_export: migration.export,
  migration_return: migration.return,
  original_remainder_return: fixture.followup.execution,
  ...(reorg && { terminal_anchor: reorg.terminal.pinned }),
})) {
  const header = certificate(row);
  const verified = header.verifyExecution(bytes(row.transaction), bytes(row.receipt), bytes(row.proof));
  if (name === "terminal_anchor") {
    assert.equal(verified.cycles, 0n);
  } else {
    assert(verified.cycles > 0n);
  }
  executions[name] = { height: Number(header.height), cycles: Number(verified.cycles) };
  const modified = bytes(row.receipt);
  modified[modified.length - 1] ^= 1;
  assert.throws(() => header.verifyExecution(bytes(row.transaction), modified, bytes(row.proof)));
}

function position(header, part) {
  const record = bytes(part.cell);
  assert(header.verifyLiveCell(bytes(part.point), record, bytes(part.proof)));
  const cell = decodeCell(record.subarray(84));
  const types = Array.from({ length: cell.typeScriptCount }, (_, index) => Buffer.from(cell.typeScript(index)));
  assert.equal(types.length, 4);
  const data = Buffer.from(cell.data());
  assert.equal(data[0], 2);
  assert.equal(data.length, 97);
  const quantities = new Map();
  for (const offset of [1, 49]) {
    const id = data.subarray(offset, offset + 32).toString("hex");
    quantities.set(id, data.readBigUInt64LE(offset + 32) | (data.readBigUInt64LE(offset + 40) << 64n));
  }
  const modified = Buffer.from(record);
  modified[modified.length - 1] ^= 1;
  assert.throws(() => header.verifyLiveCell(bytes(part.point), modified, bytes(part.proof)));
  return { types, quantities: migration.same_asset_ids.map((id) => quantities.get(bytes(id).toString("hex"))) };
}

const states = {};
const original = [100n, 250n];
const migrated = [60n, 0n];
const views = [
  ["concurrent_escrows", migration.dual_escrow, [original, migrated]],
  ["escrows_during_outage", migration.outage.held, [original, migrated]],
  ["native_settlement", migration.final_positions, [migrated, original]],
];
if (reorg) {
  views.push(["escrows_after_burn_reorg", reorg.held, [original, migrated]]);
  views.push(["settlement_after_anchor_halt", reorg.terminal.final_positions, [migrated, original]]);
}
for (const [name, row, expected] of views) {
  const header = certificate(row);
  const positions = row.positions.map((part) => position(header, part));
  assert.equal(positions.length, 2);
  assert.deepEqual(positions[0].types, positions[1].types);
  assert.deepEqual(positions.map(({ quantities }) => quantities), expected);
  assert.deepEqual(positions[0].quantities.map((quantity, i) => quantity + positions[1].quantities[i]), [160n, 250n]);
  states[name] = Number(header.height);
}

const id = bytes(migration.inbound.id);
const pendingClaims = [migration.outage.pending];
if (reorg) pendingClaims.push(reorg.pending);
for (const pending of pendingClaims) {
  assert.equal(pending.record, null);
  assert.equal(certificate(pending).verifyIntercell(true, id, bytes(null), bytes(pending.proof)), false);
}
const inbox = migration.inbox;
assert(certificate(inbox).verifyIntercell(true, id, bytes(inbox.record), bytes(inbox.proof)));
if (reorg) {
  const { committed, anchor_height, canonical_sibling, orphaned_anchor } = reorg.terminal;
  assert.equal(committed.hash, orphaned_anchor);
  const header = certificate(committed);
  const record = Buffer.alloc(48);
  record.writeBigUInt64LE(BigInt(anchor_height));
  bytes(orphaned_anchor).copy(record, 8);
  record.writeBigUInt64LE(BigInt(committed.adopted_at), 40);
  assert(BigInt(committed.adopted_at) <= header.height);
  const key = Buffer.from("bridge/anchor");
  header.verifySystemRecord(key, record, bytes(committed.proof));
  bytes(canonical_sibling).copy(record, 8);
  assert.throws(() => header.verifySystemRecord(key, record, bytes(committed.proof)));
}
console.log(JSON.stringify({ executions, states }, null, 2));

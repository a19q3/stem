const assert = require("node:assert/strict");
const { createHash } = require("node:crypto");
const { readFile } = require("node:fs/promises");
const { createRequire } = require("node:module");
const { resolve } = require("node:path");
const { openClient } = require("./ccc_client.cjs");

async function main() {
  const [sdkPrefix, configPath] = process.argv.slice(2);
  const sdk = createRequire(resolve(sdkPrefix, "package.json"));
  const { ccc } = sdk("@ckb-ccc/core");
  const { spore } = sdk("@ckb-ccc/spore");
  const { packRawSporeData } = sdk("@ckb-ccc/spore/advanced");
  const config = JSON.parse(await readFile(configPath, "utf8"));
  const client = await openClient(ccc, config);

  async function submit(tx) {
    const hash = await client.sendTransaction(tx, "passthrough");
    assert.equal(hash, tx.hash());
    const receipt = await client.waitTransaction(hash, 0, 25_000, 200);
    assert.equal(receipt?.status, "committed");
    const bytes = tx.toBytes();
    const id = createHash("sha256").update(bytes).digest("hex");
    return { id, height: Number(receipt.blockNumber), hash, packed: ccc.hexFrom(bytes) };
  }

  const fee = 1000000n;
  // These well-known keys are used in the synthetic development fixture.
  const owner = new ccc.SignerCkbPrivateKey(client, "0x" + "01".repeat(32));
  const buyer = new ccc.SignerCkbPrivateKey(client, "0x" + "02".repeat(32));
  const buyerLock = (await buyer.getRecommendedAddressObj()).script;
  const scriptInfo = {
    codeHash: config.sporeCode,
    hashType: "data1",
    cellDeps: [{ cellDep: { outPoint: config.sporeDep, depType: "code" } }],
    cobuild: true,
  };
  const funding = ccc.OutPoint.from(config.funding);
  const input = await client.getCellLiveNoCache(funding, true);
  assert(input);
  const content = ccc.hexFrom(Buffer.from("Fern Spore"));
  const { tx: mint, id } = await spore.createSpore({
    signer: owner,
    scriptInfo,
    tx: { inputs: [{ previousOutput: funding }] },
    data: { contentType: "text/plain", content },
  });
  mint.outputs[0].capacity = input.cellOutput.capacity - fee;
  const minted = await submit(await owner.signOnlyTransaction(mint));
  const found = await spore.findSpore(client, id, [scriptInfo]);
  assert(found && found.cell.cellOutput.type.args === id);
  assert.equal(found.cell.outPoint.txHash, minted.hash);
  assert.equal(found.sporeData.content, content);
  const cell = found.cell;

  const { tx: transfer } = await spore.transferSpore({ signer: owner, id, to: buyerLock, scripts: [scriptInfo] });
  transfer.outputs[0].capacity = cell.cellOutput.capacity - fee;
  const changed = transfer.clone();
  changed.setOutputData(0, packRawSporeData({ contentType: "text/plain", content: "0x00" }));
  const invalid = await owner.signOnlyTransaction(changed);
  let rejected;
  await assert.rejects(() => client.sendTransaction(invalid, "passthrough"), (error) => {
    rejected = error.data ?? String(error);
    return error.code === -302 && rejected.includes("ValidationFailure") && rejected.includes(config.sporeCode.slice(2));
  });
  const transferred = await submit(await owner.signOnlyTransaction(transfer));
  const next = await spore.findSpore(client, id, [scriptInfo]);
  assert(next && next.cell.cellOutput.lock.eq(buyerLock));
  assert.equal(next.cell.outPoint.txHash, transferred.hash);
  assert(next.cell.cellOutput.type.eq(cell.cellOutput.type));
  assert.equal(next.cell.outputData, cell.outputData);
  assert.equal(await client.getCellLiveNoCache(cell.outPoint, false), undefined);

  const { tx: melt } = await spore.meltSpore({ signer: buyer, id, scripts: [scriptInfo] });
  melt.addOutput({ lock: buyerLock, capacity: next.cell.cellOutput.capacity - fee });
  const melted = await submit(await buyer.signOnlyTransaction(melt));
  assert.equal(await spore.findSpore(client, id, [scriptInfo]), undefined);
  const returned = await client.getCellLiveNoCache({ txHash: melted.hash, index: 0 }, true);
  assert(returned && returned.cellOutput.lock.eq(buyerLock) && !returned.cellOutput.type);
  assert.equal(returned.cellOutput.capacity, input.cellOutput.capacity - 3n * fee);

  console.log(JSON.stringify({
    id, minted, transferred, melted,
    rejectContentChange: { packed: ccc.hexFrom(invalid.toBytes()), log: rejected },
    ccc: sdk("@ckb-ccc/core/package.json").version,
    cccSpore: sdk("@ckb-ccc/spore/package.json").version,
  }));
}

main().catch((error) => { console.error(error); process.exitCode = 1; });

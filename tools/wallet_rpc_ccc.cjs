const assert = require("node:assert/strict");
const { readFile } = require("node:fs/promises");
const { createRequire } = require("node:module");
const { resolve } = require("node:path");
const { openClient } = require("./ccc_client.cjs");

async function main() {
  const [sdkPrefix, configPath] = process.argv.slice(2);
  const sdk = createRequire(resolve(sdkPrefix, "package.json"));
  const { ccc } = sdk("@ckb-ccc/core");
  const config = JSON.parse(await readFile(configPath, "utf8"));
  const client = await openClient(ccc, config);

  // The local unbridged fixture funds these public development keys.
  const owner = new ccc.SignerCkbPrivateKey(client, "0x" + "01".repeat(32));
  const buyer = new ccc.SignerCkbPrivateKey(client, "0x" + "02".repeat(32));
  const ownerLock = (await owner.getRecommendedAddressObj()).script;
  const buyerLock = (await buyer.getRecommendedAddressObj()).script;
  const search = { script: ownerLock, scriptType: "lock", scriptSearchMode: "exact", withData: false };
  const first = await client.findCellsPagedNoCache(search, "asc", 1);
  const second = await client.findCellsPagedNoCache(search, "asc", 2, first.lastCursor);
  const descending = await client.findCellsPagedNoCache(search, "desc", 3);
  const points = [...first.cells, ...second.cells].map((cell) => cell.outPoint.index.toString());
  assert.deepEqual(points, ["1", "2", "3"]);
  assert.deepEqual(descending.cells.map((cell) => cell.outPoint.index.toString()), ["3", "2", "1"]);
  assert.equal(await client.getCellsCapacity(search), ccc.fixedPointFrom(300));

  const payment = ccc.Transaction.from({});
  payment.addOutput({ capacity: ccc.fixedPointFrom(130), lock: buyerLock });
  assert.equal(await payment.completeInputsByCapacity(owner), 2);
  await payment.completeFeeBy(owner, 1000n);
  const signed = await owner.signTransaction(payment);
  const sent = await client.sendTransaction(signed, "passthrough");
  assert.equal(sent, signed.hash());
  const committed = await client.waitTransaction(sent, 0, 25_000, 200);
  assert.equal(committed?.status, "committed");
  const output = await client.getCellLiveNoCache({ txHash: sent, index: 0 }, true);
  assert(output?.cellOutput.lock.eq(buyerLock));
  assert.equal(output.cellOutput.capacity, ccc.fixedPointFrom(130));
  for (const input of signed.inputs) {
    assert.equal(await client.getCellLiveNoCache(input.previousOutput, false), undefined);
  }
  const origin = await client.getHeaderByNumber(committed.blockNumber);
  assert.equal(origin?.hash, committed.blockHash);
  const block = await client.getBlockByNumber(committed.blockNumber);
  assert.equal(block?.header.hash, committed.blockHash);
  assert(block.transactions.some((transaction) => transaction.hash() === sent));

  const current = await client.findCellsPagedNoCache(search, "asc", 10);
  assert.equal(current.cells.length, 2);
  assert(current.cells.every((cell) => !signed.inputs.some((input) => input.previousOutput.eq(cell.outPoint))));
  console.log(JSON.stringify({
    sdk: sdk("@ckb-ccc/core/package.json").version,
    genesisCellsFound: points, inputCount: signed.inputs.length, remainingOwnerCells: current.cells.length,
    hash: sent, packed: ccc.hexFrom(signed.toBytes()),
    blockHash: committed.blockHash, blockNumber: committed.blockNumber.toString(),
  }));
}

main().catch((error) => { console.error(error); process.exitCode = 1; });

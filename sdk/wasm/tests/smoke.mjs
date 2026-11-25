import * as sdk from "./stem_wasm.js";

const bytes = (value) => {
    const raw = value.replace(/^0x/, "");
    return Uint8Array.from(raw.match(/.{2}/g) ?? [], (byte) => Number.parseInt(byte, 16));
};
const hex = (value) => Array.from(value, (byte) => byte.toString(16).padStart(2, "0")).join("");

function equal(actual, expected, label) {
    if (actual !== expected) {
        throw new Error(`${label}: ${actual} differs from ${expected}`);
    }
}

function rejects(invoke, label) {
    try {
        invoke();
    } catch {
        return label;
    }
    throw new Error(`${label}: accepted invalid input`);
}

function changed(raw) {
    const corrupt = raw.slice();
    corrupt[corrupt.length - 1] ^= 1;
    return corrupt;
}

function verifier(trust) {
    return new sdk.StemVerifier(bytes(trust.network_id), bytes(trust.genesis_id), bytes(trust.initial_committee));
}

function verified(owner, data) {
    const header = owner.verifyFinality(bytes(data.finality), false);
    equal(header.height, BigInt(data.header.height), "finalized height");
    equal(hex(header.appHash()), data.header.app_hash, "finalized app hash");
    return header;
}

function packPoint(point) {
    const raw = new Uint8Array(36);
    raw.set(bytes(point.tx_hash));
    new DataView(raw.buffer).setUint32(32, Number(point.index), true);
    return raw;
}

export function run(fixture, handoff) {
    const start = performance.now();
    const { trust, signing, execution, spent, outbox, vault_revenue: vault } = fixture;
    const owner = verifier(trust);
    const rejected = [];

    const tip = verified(owner, signing);
    const context = new sdk.SigningContext(tip);
    for (let index = 0; index < signing.cells.length; index++) {
        const { cell, proof } = signing.cells[index];
        equal(tip.verifyLiveCell(bytes(signing.points[index]), bytes(cell), bytes(proof)), true, "live signing Cell");
        context.addLiveCell(bytes(cell), bytes(proof));
    }
    const lock = sdk.packScript(bytes(signing.lock.code_hash), signing.lock.hash_type === "type", bytes(signing.lock.args));
    const builder = new sdk.TransactionBuilder();
    for (const input of signing.request.inputs) builder.addInput(packPoint(input.out_point));
    for (const dep of signing.request.cell_deps) builder.addCellDep(packPoint(dep.out_point), dep.dep_type === "dep_group");
    for (const raw of signing.request.outputs) {
        const decoded = sdk.decodeCell(bytes(raw));
        const rebuilt = new sdk.CellBuilder(decoded.capacity, decoded.lock(), decoded.data());
        for (let i = 0; i < decoded.typeScriptCount; i++) rebuilt.addTypeScript(decoded.typeScript(i));
        builder.addOutput(rebuilt.build());
    }
    for (const group of signing.request.messages) {
        for (const action of group.actions) builder.addAction(group.group_index, action.kind, bytes(action.data));
    }
    const unsigned = builder.build();
    equal(hex(unsigned.encode()), hex(bytes(signing.unsigned)), "browser encodes the canonical transaction");
    const digest = context.signingDigest(unsigned, 0, lock);
    equal(hex(digest), hex(bytes(signing.digest)), "native signing digest");
    const signed = sdk.decodeTransaction(bytes(signing.signed));
    equal(hex(unsigned.withSeal(0, signed.seal(0)).encode()), hex(signed.encode()), "browser assembles the native seal");
    rejected.push(rejects(() => new sdk.SigningContext(tip).signingDigest(unsigned, 0, lock), "missing signing inputs"));

    const proven = verified(owner, execution);
    const receipt = proven.verifyExecution(bytes(execution.transaction), bytes(execution.receipt), bytes(execution.proof));
    equal(hex(receipt.transactionId()), execution.id, "executed transaction identity");
    equal(hex(signed.id(bytes(trust.network_id))), execution.id, "decoded transaction identity");
    rejected.push(rejects(() => proven.verifyExecution(bytes(execution.transaction), changed(bytes(execution.receipt)),
        bytes(execution.proof)), "changed receipt"));
    rejected.push(rejects(() => tip.verifyExecution(bytes(execution.transaction), bytes(execution.receipt),
        bytes(execution.proof)), "wrong execution Header"));

    const missing = verified(owner, spent);
    equal(missing.verifyLiveCell(bytes(signing.points[0]), new Uint8Array(), bytes(spent.proof)), false, "spent Cell non-membership");
    rejected.push(rejects(() => missing.verifyLiveCell(bytes(signing.points[0]), bytes(signing.cells[0].cell),
        bytes(spent.proof)), "spent record supplied as live"));

    const outboxHeader = verified(owner, outbox);
    equal(outboxHeader.verifyIntercell(false, bytes(outbox.id), bytes(outbox.record), bytes(outbox.proof)), true, "Intercell send");
    equal(hex(sdk.intercellMessageId(bytes(outbox.record))), hex(bytes(outbox.id)), "Intercell identity");
    rejected.push(rejects(() => outboxHeader.verifyIntercell(false, bytes(outbox.id), changed(bytes(outbox.record)),
        bytes(outbox.proof)), "changed Intercell payload"));

    const vaultHeader = verified(owner, vault);
    const accounting = vault.accounting;
    const amount = accounting.extinguished_shannons === null ? undefined : BigInt(accounting.extinguished_shannons);
    const revenue = vaultHeader.verifyVaultRevenue(bytes(accounting.record), bytes(accounting.proof), amount,
        bytes(accounting.fee_proof));
    equal(revenue.total, BigInt(accounting.attributed_shannons), "vault revenue");
    equal(revenue.laneCount, accounting.entries.length, "vault count");
    for (let i = 0; i < revenue.laneCount; i++) {
        equal(hex(revenue.laneId(i)), hex(bytes(accounting.entries[i].lane)), "lane identity");
        equal(revenue.attributedFees(i), BigInt(accounting.entries[i].attributed_shannons), "lane amount");
    }
    rejected.push(rejects(() => vaultHeader.verifyVaultRevenue(bytes(accounting.record), bytes(accounting.proof),
        (amount ?? 0n) + 1n, bytes(accounting.fee_proof)), "forged vault income"));

    rejected.push(rejects(() => owner.verifyFinality(changed(bytes(signing.finality)), false), "changed quorum certificate"));
    const other = verifier({ ...trust, network_id: "ff".repeat(32) });
    rejected.push(rejects(() => other.verifyFinality(bytes(signing.finality), false), "foreign network"));

    const rotating = verifier(handoff.trust);
    rejected.push(rejects(() => rotating.verifyFinality(bytes(handoff.successor.finality), false), "missed committee handoff"));
    for (const change of handoff.changes) {
        const boundary = rotating.acceptHandoff(bytes(change.proof));
        equal(boundary.height, BigInt(change.header.height), "handoff height");
        equal(hex(boundary.appHash()), change.header.app_hash, "handoff app hash");
        rejected.push(rejects(() => rotating.acceptHandoff(bytes(change.proof)), "replayed committee handoff"));
    }
    const successor = verified(rotating, handoff.successor);
    rejected.push(rejects(() => context.addHeader(successor), "foreign signing trust anchor"));

    const first = sdk.packScript(new Uint8Array(32).fill(1), false, Uint8Array.of(1));
    const second = sdk.packScript(new Uint8Array(32).fill(2), false, Uint8Array.of(2));
    const multi = new sdk.CellBuilder(10_000_000_000n, lock, Uint8Array.of(3));
    multi.addTypeScript(second);
    multi.addTypeScript(first);
    const multiCell = sdk.decodeCell(multi.build().encode());
    equal(multiCell.typeScriptCount, 2, "Polytype codec");
    equal(hex(multiCell.typeScript(0)), hex(first), "canonical Script ordering");

    return {
        transaction_bytes: signed.encode().length,
        transaction_id: execution.id,
        fee: String(receipt.fee),
        cycles: String(receipt.cycles),
        applied_handoffs: handoff.changes.length,
        successor_height: String(successor.height),
        vault_revenue: String(revenue.total),
        rejected,
        verification_ms: performance.now() - start,
    };
}

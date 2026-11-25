import {
    AuthenticatedHeader,
    CellBuilder,
    DecodedTransaction,
    SigningContext,
    StemVerifier,
    TransactionBuilder,
} from "./stem_wasm.js";

export function checkTypes(network: Uint8Array, genesis: Uint8Array, roster: Uint8Array,
    proof: Uint8Array, point: Uint8Array, lock: Uint8Array): bigint {
    const verifier = new StemVerifier(network, genesis, roster);
    const header: AuthenticatedHeader = verifier.verifyFinality(proof, false);
    const signing = new SigningContext(header);
    const builder = new TransactionBuilder();
    builder.addInput(point);
    builder.addOutput(new CellBuilder(10_000_000_000n, lock, new Uint8Array()).build());
    const draft: DecodedTransaction = builder.build();
    const digest: Uint8Array = signing.signingDigest(draft, 0, lock);
    return header.height + BigInt(digest.length);
}

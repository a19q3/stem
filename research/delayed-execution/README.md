# Fixed-delay execution experiment

The offline experiment tested a Monad-style three-height delay on Stem's native Cell execution. Stem V1 continues to certify the full execution Header through weighted Jolteon and to publish that proof through the CKB settlement adapter. This report records the retired experiment and its results. [V1 execution roadmap](../../STEM-EVOLUTION.md#stem-v1-执行路线)

## Model

An ordered block bound its parent ordering ID, height, timestamp and Merkle root of the complete signed transactions. At height `h`, its certificate also committed to the ordered block and native execution Header at height `h−3`. The first three heights carried a pinned genesis execution marker. Two consecutive weighted certificates finalized an ordering block and authenticated its delayed execution commitment. The proof cursor checked the earlier payload root, order ID, timestamp, height and previous execution Header. It held at most three finalized ordering blocks awaiting their execution commitments.

## Results

The fixture ran seven signed Cell spends through the native Store. A four-member fixed committee signed each certificate with three Ed25519 keys.

| Check | Result |
| --- | --- |
| Startup | Ordering advanced during the first three heights before releasing a new execution commitment. |
| Execution deadline | The proof cursor rejected an advance when the required execution Header was missing. |
| Proof binding | The verifier rejected changes to the state root and commitments to a different ordered block. After the required commitments arrived, it verified native transaction and receipt proofs. |
| Compatibility | The native finality verifier and CKB checkpoint path accepted the execution certificate and rejected the experimental ordering certificates. |
| Conflicting Cell spends | Two individually valid spends of the same Cell caused native block preparation to reject the pair. The model covered valid blocks. It established no rule for recording or charging failed transactions after ordering. |

A separate native control exercised the existing CKB checkpoint path. Four Stem replicas finalized 48 payments with the bridge disabled. A subsequent height-15 checkpoint was accepted on a two-node CKB devnet after P2P relay, with 18,377,024 estimated CKB cycles. A CKB-funded devnet published committee handoffs at heights 10 and 20 and an execution checkpoint at height 25. The RISC-V verifier rejected a modified execution root and a replayed handoff. Offline WASM replay verified all four execution Headers and matched them to the bytes in the signed CKB transactions. It rejected post-handoff finality when the handoffs were omitted.

The delayed model established proof binding for a fixed committee in an offline fixture. Live ordered-payload availability, crash recovery, committee changes, Cell failure and resource accounting, and network throughput remained untested for this protocol.

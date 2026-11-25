# Telegraph compiler and runtime

The standalone schema compiler and borrowed Rust runtime derive from the
[v10 reference commit](https://github.com/Tondi-Foundation/Tondi/tree/7db6d0c4fa4eec07937c98fbc69ecdc884569e50).
The upstream files are `vm/telegraph/generate.py` and `vm/src/telegraph.rs`.
They are distributed under the accompanying [ISC license](LICENSE).

Stem supplies its own schema and requests standalone Rust and C outputs. The
compiler does not read the upstream workspace, schemas or VM definitions.
The Rust runtime uses the same byte buffer type as the ledger.

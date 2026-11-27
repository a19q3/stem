# Ordinary Cluster members issued on Stem

A Stem-issued Spore can name an ordinary Cluster and use the published mainnet Spore V2 program on CKB. The first CKB mint supplies the collection and a transaction authorized by its owner. The native title commits to the complete Spore bytes, the reserved CKB Type and the settlement endpoint. A wallet can then use the CKB original to authorize a direct change of native owner. The bound checks include the prior native input, successor Cell and original content digest. Inbound observations use the configured Stem guardians. [Operator instructions](../../../../OPERATIONS.md#exercise-ckb-spore-settlement)

## Public deployment and collection data

The two historical deployments use the same Cluster binary, with data hash `7366a61534fa7c7e6225ecc0d828ea3b5366adec2b58206f2ee84995fe030075`. The queries include spent outputs through the indicated heights. Compact live outputs supplied raw data directly. For remaining outputs the scan fetched their committed creating transactions. The end header was checked again after enumeration. Counts depend on the queried indexer's completeness. [Upstream releases](https://github.com/sporeprotocol/spore-contract/blob/9e577ea6b5f2fb9cbf555e9e86effcc037c78039/docs/VERSIONS.md) · [Mainnet rows and anchor](mainnet.json.xz) · [Testnet rows and anchor](testnet.json.xz)

| Chain and addressing | Height | Historical outputs | Distinct collection IDs | Three fields, empty third | Four fields, empty third |
| --- | ---: | ---: | ---: | ---: | ---: |
| Mainnet Data1 | 21,119,521 | 1,853 | 492 | 1,853 | 0 |
| Testnet Data1 | 23,195,425 | 776 | 487 | 773 | 3 |
| Testnet deployment Type | 23,195,425 | 0 | 0 | 0 | 0 |

For all three four-field records, the offline wallet verified the creating transaction and its Cluster executable against the archived creation-block header. The fourth field contains a length-prefixed IPFS URI. Two distinct IDs shared the same 124-byte record. The third record was 91 bytes and had a different digest. Its exact bytes served as collection data in three funded devnets. Those runs issued fresh IDs. The public records establish the observed layout and bytes. The separate case of a nonempty collection-level Mutant still needs a successful original-program issuance. [Three public history proofs](four-field-histories.json.xz) · [Upstream Mutant specification](https://github.com/sporeprotocol/spore-contract/blob/9e577ea6b5f2fb9cbf555e9e86effcc037c78039/docs/MUTANT.md) · [Earlier beta.2 deployment scan](../beta2/README.md#cluster-membership-and-collection-level-registration)

```mermaid
flowchart LR
    P["Authenticated public collection data"] -->|same bytes, new ID| C["Local CKB collection"]
    N["Stem issues title"] --> E["Escrow native title"]
    C -->|collection owner's authorization| M["CKB mints standard member"]
    E --> M
    M --> W["CKB wallet changes owner"]
    W --> R["Return to Stem"]
    R -->|optional CKB owner grants| H["Change native owner"]
    H --> X["Re-export same CKB Type"]
    X -->|authenticated melt receipt| F["Retire native title and refund"]
```

## Settlement and compatibility

Six independent CKB devnets used four local Jolteon replicas apiece. Native-origin ordinary members used tag 6 on both the native identity and the custody endpoints. At first export the original CKB contract accepted the collection owner's authorization. Registered beta.2 members retained their identity tag 4 and gate tag 3. On the import run the ordinary collection transferred to a second CKB owner between deposit and release. The referenced collection version at issuance and its later owner were verified at the same anchor as the member history. [Settlements and witnessed CKB transactions](settlements.json.xz) · [Public native trust roots](committee-trust.json)

| Run | Member transitions on CKB | Exact collection references checked | Certified Stem executions | Common Stem height | Outcome |
| --- | ---: | ---: | ---: | ---: | --- |
| Ordinary native member | 5 | 5 | 4 | 33 | Re-export and wallet melt |
| Native member with two grants | 7 | 7 | 7 | 52 | CKB melt, native retirement and refund |
| Immortal native member | 6 | 6 | 6 | 42 | Two grants and live re-export |
| Registered beta.2 native member | 6 | 6 | 6 | 48 | Two grants and live re-export |
| Unclustered native Spore | 5 | 0 | 5 | 36 | CKB melt, native retirement and refund |
| Ordinary member issued on CKB | 4 | 1 | 4 | 35 | Collection changed owner, member redeemed and reimported |

The standard wallet melt in the first run left the native original in custody. The synchronized path also delivered the terminal receipt and produced the full tagged refund on Stem. The WASM verifier rechecked membership of the refund and absence of the spent original at retirement, and verified native execution against its initial committee. [Native proof replay](../beta2/native.mjs)

## Replay

From the repository root, use the release wallet and the built WASM SDK directory.

```sh
study=research/spore-permit/upstream/collections
python3 "$study/check.py" --stem target/release/stem
node "$study/../beta2/native.mjs" "$sdk_directory" "$study"
```

To regenerate the public evidence, select a new output directory. The collection index uses the public JSON-RPC endpoints for each chain. The three short histories can also be verified fully offline from the archive.

```sh
python3 "$study/migrations.py" mainnet --height 21119521 --output "$out/mainnet.json.xz"
python3 "$study/migrations.py" testnet --height 23195425 --output "$out/testnet.json.xz"
python3 "$study/public.py" --stem target/release/stem --index "$out/testnet.json.xz" --output "$out/four-field-histories.json.xz"
```

The local fixture accepts packed collection bytes with `--spore-cluster-data`. With the six report directories named as in [the archive collector](pack.py), collect their public histories, settlement results and independently exported genesis committees with `python3 "$study/pack.py" --runs "$runs" --stem target/release/stem --output "$out"`.

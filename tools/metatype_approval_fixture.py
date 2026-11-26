"""Construct approval registry updates and detached signatures for the devnet."""

from bridge_localnet import CKB, FEE, binary, ckb_hash, hex_bytes, le, output, packed_script, point
from bridge_localnet import script, script_hash, transaction, wallet_group


def type_scripts(cell):
    raw = binary(cell)
    assert raw[0] in (0, 1)
    if not raw[0]:
        return set()
    start = 46 + int.from_bytes(raw[42:46], "little")
    count = int.from_bytes(raw[start:start + 4], "little")
    start += 4
    scripts = set()
    for _ in range(count):
        size = int.from_bytes(raw[start + 33:start + 37], "little")
        scripts.add(packed_script(script(hex_bytes(raw[start:start + 32]),
            hex_bytes(raw[start + 37:start + 37 + size]), {0: "data", 1: "type"}[raw[start + 32]])))
        start += 37 + size
    assert len(scripts) == count
    return scripts


class Rotation:
    def __init__(self, fixture, genesis, stem, code, seed):
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        self.fixture = fixture
        self.genesis = binary(genesis)
        self.stem = binary(stem)
        self.keys = [Ed25519PrivateKey.from_private_bytes(bytes([index]) * 32) for index in (41, 42)]
        identity = ckb_hash(le(0, 8) + binary(seed["tx_hash"]) + le(int(seed["index"], 16), 4) + le(0, 8))
        self.registry = script(hex_bytes(code), hex_bytes(identity), "data2")
        self.generation = 0
        self.freeze_by_generation = {0: False, 1: False}
        self.location = seed
        self.capacity = 450 * CKB

    def record(self, generation, frozen=None):
        if frozen is None:
            frozen = self.freeze_by_generation[generation]
        key = self.keys[int(generation != 0)]
        return le(generation, 8) + key.public_key().public_bytes_raw() + bytes([frozen])

    def update(self, label, generation, deps, *, data=None, lock=None, frozen=None):
        transfer = transaction([self.location],
            [output(self.capacity - FEE, lock or self.fixture.standard, self.registry)],
            [hex_bytes(self.record(generation, frozen) if data is None else data)], deps)
        return self.fixture.sign(f"object-approval-registry-{label}", transfer,
                                 [wallet_group(0, [self.fixture.faucet])])

    def applied(self, tx_hash, generation, frozen=False):
        self.location = point(tx_hash, 0)
        self.generation = generation
        self.freeze_by_generation[generation] = frozen
        self.capacity -= FEE

    def sign(self, transaction_id, generation, *, deployment=None):
        network = self.stem if deployment is None else self.genesis
        scope = bytes(32) if deployment is None else deployment
        digest = ckb_hash(b"Stem Metatype rotating approval\0" + network + scope
                          + script_hash(self.rule) + self.record(generation) + binary(transaction_id))
        return self.keys[int(generation != 0)].sign(digest)

    def ckb_sign(self, label, tx, signers, deployment, selected, workflow_row=()):
        from metatype_object_localnet import encode

        name = f"object-{label}"
        draft = self.fixture.sign(f"{name}-approval-draft", tx, [])
        rows = list(workflow_row)
        if selected is not False:
            generation = self.generation if selected is None else selected
            rows.append((deployment, script_hash(self.rule), 1, b"", b"",
                         self.sign(draft["hash"], generation, deployment=deployment)))
        witnesses = tx.get("witnesses", [])
        tx["witnesses"] = [*witnesses, *(["0x"] * (len(tx["inputs"]) - len(witnesses)))]
        if rows:
            tx["witnesses"].append(hex_bytes(encode(rows)))
        signed = self.fixture.sign(name, tx, signers)
        assert signed["hash"] == draft["hash"]
        return signed

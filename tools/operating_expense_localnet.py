"""Exercise a one-shot, transaction-specific CKB operating expense reimbursement."""

import json

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, output, point, script_hash, transaction, wallet_group
from localnet import run, write_json


class ExpenseBudget:
    def __init__(self, scenario, program, funding=None):
        self.scenario = scenario
        self.fixture, self.l1 = scenario.fixture, scenario.l1
        code = program.read_bytes()
        self.hash = hex_bytes(ckb_hash(code))
        code_capacity = (len(code) + 100) * CKB
        outputs = [output(code_capacity, self.fixture.standard)]
        values = [hex_bytes(code)]
        total = code_capacity
        if funding is None:
            self.refund, self.authorizer = self.fixture.friend_lock, self.fixture.friend
            self.capacity = 1500 * CKB
            outputs.append(output(self.capacity, self.refund))
            values.append("0x")
            total += self.capacity
        else:
            self.treasury, self.refund, self.authorizer = funding
            funded = self.l1.call("get_live_cell", self.treasury, True)["cell"]["output"]
            assert funded["lock"] == self.refund
            self.capacity = int(funded["capacity"], 16)
        deployed = scenario.accept("expense-budget", scenario.pay_transaction([], outputs, values, [], change_delta=-total))
        self.code = dependency(point(deployed, 0))
        if funding is None:
            self.treasury = point(deployed, 1)

    def settle(self, receipt, evidence_path):
        s, fixture, l1 = self.scenario, self.fixture, self.l1
        request = s.args.output / "expense-grant-request.json"
        write_json(request, {"l1_genesis": s.genesis, "trusted_block": receipt["block"],
            "transaction": receipt["transaction"], "payer_lock": receipt["payer_lock"],
            "refund_lock": hex_bytes(script_hash(self.refund)), "grant_code": self.hash,
            "max_expense_fee_shannons": FEE, "max_settlement_fee_shannons": FEE})
        prepared = json.loads(run(s.args.binary, "checkpoint", "fund", "expense-grant",
            "--request", request, "--evidence", evidence_path))
        assert prepared["expense_change"] == s.wallet and prepared["receipt"] == receipt
        lock = prepared["grant_lock"]
        amount = 350 * CKB
        reserve = self.capacity - 2*amount - FEE
        auth = transaction([self.treasury], [output(amount, lock), output(amount, lock), output(reserve, self.refund)],
                           ["0x"] * 3, [s.secp])
        approved = l1.send(fixture.sign("authorize-individual-reimbursement", auth, [wallet_group(0, [self.authorizer])]))
        grant, duplicate, treasury = [point(approved, i) for i in range(3)]
        deps = [s.secp, self.code]
        fee = receipt["miner_fee_shannons"]
        payer_before = s.wallet_capacity

        def claim(label, *, authorization=grant, paid=fee, tx_fee=FEE, recipient=s.release_lock, proof=prepared["witness"]):
            tx = transaction([prepared["expense_change"], authorization],
                [output(payer_before+paid, recipient), output(amount-paid-tx_fee, self.refund)], ["0x", "0x"], deps)
            tx["witnesses"] = ["0x", proof]
            return fixture.sign(label, tx, [wallet_group(0, [fixture.faucet])])

        rejected = {}
        for label, expected, modified in [
            ("overpayment", 123, {"paid": fee+1}), ("underpayment", 123, {"paid": fee-1}),
            ("excess-claim-fee", 123, {"tx_fee": FEE+1}), ("redirected", 121, {"recipient": self.refund}),
            ("missing-ancestry", 122, {"proof": "0x"}),
        ]:
            rejected[label] = l1.reject(claim(label, **modified))
            assert f"error code {expected} " in rejected[label], rejected[label]

        signed = claim("reimburse-miner-fee")
        replay = claim("repeat-with-another-grant", authorization=duplicate)
        cycles = int(l1.call("estimate_cycles", signed["json"])["cycles"], 16)
        settled = l1.send(signed)
        s.wallet, s.wallet_capacity = point(settled, 0), payer_before+fee
        assert l1.call("get_live_cell", prepared["expense_change"], True)["status"] != "live"
        try:
            l1.call("send_transaction", replay["json"], "passthrough")
        except RuntimeError as error:
            rejected["spent-change-on-second-grant"] = str(error)
            assert any(term in str(error).lower() for term in ("dead", "unknown", "resolve")), error
        else:
            raise AssertionError("second grant reimbursed the same expense")
        cancel = transaction([treasury, duplicate], [output(reserve+amount-FEE, self.refund)], ["0x"], deps)
        cancelled = l1.send(fixture.sign("recover-redundant-grant", cancel, [wallet_group(0, [self.authorizer])]))
        for location, expected_cap, expected_lock in [(s.wallet, payer_before+fee, s.release_lock),
                (point(settled, 1), amount-fee-FEE, self.refund), (point(cancelled, 0), reserve+amount-FEE, self.refund)]:
            assert l1.call("get_live_cell", location, True)["cell"]["output"] == output(expected_cap, expected_lock)
        return {"treasury_input": self.treasury, "authorizer_lock": self.refund, "pinned_expense": receipt, "grant_lock": lock, "expense_change": prepared["expense_change"],
                "authorization": {"tx": auth, "inclusion": l1.prove_committed(approved)},
                "settlement": {"signed": signed, "inclusion": l1.prove_committed(settled), "cycles": cycles},
                "cancelled_duplicate": {"transaction": cancelled, "inclusion": l1.prove_committed(cancelled)},
                "net_payer_gain_shannons": fee, "grant_debit_shannons": 2*FEE, "rejections": rejected}

"""Exercise the CKB Lock that funds sparse checkpoint transactions."""

import copy
import json
import subprocess

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, output, point, transaction, wallet_group
from jolteon_checkpoint_localnet import PublisherFixture
from localnet import run, write_json
from native_bridge_localnet import wait_for
from stake_localnet import witness


def deploy_fee_lock(args, l1, fixture, vault, *, interval_ms=3000):
    raw = args.checkpoint_fee_fund.read_bytes()
    anchor = vault["anchor"]
    balance = int(l1.call("get_live_cell", vault["change_point"], True)["cell"]["output"]["capacity"], 16)
    capacity = (len(raw) + 100) * CKB
    tx = transaction([vault["change_point"]],
                     [output(capacity, fixture.standard), output(balance - capacity - FEE, fixture.standard)],
                     [hex_bytes(raw), "0x"], [vault["secp"]])
    deployment = l1.send(fixture.sign("deploy-checkpoint-fee-fund", tx, [wallet_group(0, [fixture.faucet])]))
    request = {
        "code_hash": hex_bytes(ckb_hash(raw)), "checkpoint_code_hash": anchor["hash"],
        "checkpoint_type_id": hex_bytes(anchor["id"]), "network": anchor["network"],
        "ckb_genesis": anchor["genesis"], "fee_cap": 2 * FEE, "interval_ms": interval_ms,
    }
    path = args.output / "checkpoint-fee-lock-request.json"
    write_json(path, request)
    lock = json.loads(run(args.binary, "checkpoint", "fund", "lock", "--request", path))
    return {"deployment": deployment, "code": point(deployment, 0), "change": point(deployment, 1),
            "lock": lock, "policy": request}


def use_fee_fund(args, l1, fixture, vault, relay, fund, payouts, wallet_fee):
    def cell_at(p):
        return l1.call("get_live_cell", p, True)["cell"]["output"]

    def capacity(p):
        return int(cell_at(p)["capacity"], 16)

    root = args.output
    deposits = [point(p["l1_payout"], 0) for p in payouts]
    principal = sum(capacity(p) for p in deposits)
    assert all(cell_at(p)["lock"] == fund["lock"] for p in deposits)
    code_dep = dependency(fund["code"])
    rejects = {}

    def reject(label, tx, error):
        write_json(root / f"checkpoint-fund-invalid-{label}.json", tx)
        message = l1.reject({"json": tx})
        assert f"error code {error}" in message, message
        rejects[label] = message

    merge = transaction(deposits + [wallet_fee],
        [output(principal, fund["lock"]), output(capacity(wallet_fee) - FEE, fixture.standard)],
        ["0x", "0x"], [code_dep, vault["secp"]])
    divert = copy.deepcopy(merge)
    divert["outputs"][0]["capacity"] = hex(principal - 1)
    reject("underfunded-merge", fixture.sign("fee-fund-bad-merge", divert, [wallet_group(2, [fixture.faucet])])["json"], 92)
    merged_tx = fixture.sign("checkpoint-fee-fund-merged", merge, [wallet_group(2, [fixture.faucet])])
    merged = l1.send(merged_tx)
    pot = point(merged, 0)
    assert capacity(pot) == principal

    cp_cap = capacity(relay.point)
    previous_height = relay.publications[-1]["height"]
    previous_time = int.from_bytes(relay.header(previous_height)[8:16], "little")

    def block_time(height):
        return int.from_bytes(relay.header(height)[8:16], "little")

    def checkpoint_tx(bundle, fee, source=None, old=None):
        source = source or relay
        tx = transaction([old or source.point, pot],
            [output(cp_cap, source.lock, source.type), output(principal - fee, fund["lock"])],
            [bundle["tip"], "0x"], [source.anchor["code"], code_dep])
        tx["witnesses"] = [witness(binary(bundle["witness"]))]
        return tx

    early = previous_height + 1
    wait_for(lambda: relay.client.status()["height"] >= early, "finalized early Header")
    assert block_time(early) - previous_time < fund["policy"]["interval_ms"]
    early_bundle = relay.bundle(early)
    reject("early", checkpoint_tx(early_bundle, FEE), 93)

    def eligible():
        height = relay.client.status()["height"]
        return height if block_time(height) >= previous_time + fund["policy"]["interval_ms"] else None

    height = wait_for(eligible, "finalized Header after the treasury interval")
    bundle = relay.bundle(height)
    unsigned = checkpoint_tx(bundle, FEE)
    reject("fee-cap", checkpoint_tx(bundle, fund["policy"]["fee_cap"] + 1), 93)
    altered = copy.deepcopy(unsigned)
    altered["outputs"][1]["lock"] = fixture.friend_lock
    reject("change-lock", altered, 92)
    altered = copy.deepcopy(unsigned)
    altered["outputs"][0]["capacity"] = hex(cp_cap + 1)
    reject("checkpoint-capacity", altered, 92)
    altered = copy.deepcopy(unsigned)
    altered["outputs"][1]["capacity"] = hex(principal - 61 * CKB - FEE)
    altered["outputs"].append(output(61 * CKB, fixture.friend_lock))
    altered["outputs_data"].append("0x")
    reject("extra-output", altered, 92)
    altered = copy.deepcopy(unsigned)
    bad = bytearray(binary(bundle["tip"]))
    bad[-1] ^= 1
    altered["outputs_data"][0] = hex_bytes(bad)
    reject("forged-checkpoint", altered, 43)

    foreign = relay.foreign(vault["foreign_anchor"])
    foreign.advance(previous_height, vault["secp"])
    reject("wrong-type-id", checkpoint_tx(foreign.bundle(height), FEE, source=foreign), 92)

    wallet_control = transaction([relay.point, relay.funding],
        [output(cp_cap, relay.lock, relay.type), output(capacity(relay.funding) - FEE, fixture.friend_lock)],
        [bundle["tip"], "0x"], [relay.anchor["code"], vault["secp"]])
    wallet_control["witnesses"] = unsigned["witnesses"]
    signed_control = fixture.sign("checkpoint-fund-wallet-control", wallet_control, [wallet_group(1, [fixture.friend])])
    control_cycles = int(l1.call("estimate_cycles", signed_control["json"])["cycles"], 16)
    publication = publish_funded_checkpoint(args, l1, relay, fund, pot, bundle, expected=unsigned)
    built = publication["funded_checkpoint"]
    final_pot = publication["successor_fund"]

    def spent(p):
        return l1.call("get_live_cell", p, True)["status"] != "live"

    assert capacity(point(built["hash"], 0)) == cp_cap
    assert principal - capacity(final_pot) == FEE and cell_at(final_pot)["lock"] == fund["lock"]
    assert spent(pot)
    return {"fund": fund, "deposits": deposits, "merged": merged, "merge_proof": l1.prove_committed(merged),
            "old_checkpoint": relay.point, "new_checkpoint": point(built["hash"], 0), "successor_fund": final_pot,
            "previous_stem_height": previous_height, "new_stem_height": height,
            "elapsed_stem_ms": block_time(height) - previous_time,
            "principal_shannons": principal, "fee_shannons": FEE, "remaining_shannons": capacity(final_pot),
            **publication, "rejections": dict(rejects, replay=publication["replay"]),
            "wallet_control": {"transaction": signed_control, "estimated_cycles": control_cycles,
                               "bytes": len(binary(signed_control["molecule"]))}}


def publish_funded_checkpoint(args, l1, relay, fund, pot, bundle, *, expected=None):
    request = {
        "checkpoint": relay.point, "checkpoint_code": relay.anchor["code"]["out_point"],
        "fund": pot, "fund_code": fund["code"], "fee": FEE, "witness": bundle["witness"],
    }
    path = args.output / "checkpoint-fund-update.json"
    write_json(path, request)
    cli = [args.binary, "checkpoint", "fund", "update", "--request", path, "--l1-rpc", l1.url]
    built = json.loads(run(*cli))
    if expected is not None:
        assert built["json"] == expected
    estimate = int(l1.call("estimate_cycles", built["json"])["cycles"], 16)
    submitted = json.loads(run(*cli, "--submit"))
    assert submitted["hash"] == built["hash"]
    for _ in range(20):
        status = l1.call("get_transaction", built["hash"])["tx_status"]["status"]
        if status == "committed":
            break
        assert status != "rejected"
        l1.mine(1)
    else:
        raise AssertionError("funded checkpoint did not commit")
    l1.mine(2)
    write_json(args.output / "checkpoint-fund-sponsored-transaction.json", built)
    replay = subprocess.run(cli + ["--submit"], text=True, capture_output=True, timeout=30)
    assert replay.returncode and "Cell is no longer live" in replay.stderr, replay
    return {"funded_checkpoint": built, "successor_fund": point(built["hash"], 1),
            "estimated_cycles": estimate, "signed_bytes": len(binary(built["molecule"])),
            "l1_inclusion": l1.prove_committed(built["hash"]), "replay": replay.stderr}


def funded_handoff(args, l1, fixture, scenario, relay):
    available = {"anchor": scenario.checkpoint, "secp": scenario.secp,
                 "change_point": scenario.deployment_change}
    fund = deploy_fee_lock(args, l1, fixture, available, interval_ms=4 * 3600 * 1000)
    change = l1.call("get_live_cell", fund["change"], True)["cell"]["output"]
    principal = 220 * CKB
    donation = transaction([fund["change"]],
        [output(principal, fund["lock"]), output(int(change["capacity"], 16) - principal - FEE, fixture.standard)],
        ["0x", "0x"], [scenario.secp])
    donor = l1.send(fixture.sign("checkpoint-fund-handoff-donation", donation, [wallet_group(0, [fixture.faucet])]))
    publisher = PublisherFixture(args, l1, fixture, relay, scenario.secp)
    bootstrap = publisher.publish("bootstrap-wallet-checkpoint", 9)
    assert bootstrap["stem_height"] == 9 and bootstrap["fee_source"] == "wallet"
    receipt = args.output / "checkpoint-fund-publisher.json"
    write_json(receipt, {"out_point": point(donor, 0), "code": fund["code"]})
    submitted = publisher.tick("submit-funded-handoff", 10, fee_fund=receipt)
    assert (submitted["status"], submitted["stem_height"], submitted["next_epoch"],
            submitted["fee_source"]) == ("submitted", 10, 1, "fund"), submitted
    restarted = publisher.tick("restart-funded-handoff")
    assert restarted["fee_source"] == "fund" and restarted["transaction"] == submitted["transaction"]
    publisher.mine_inclusion(submitted["transaction"])
    confirmed = publisher.confirm("confirm-funded-handoff", submitted)
    previous_tx = l1.call("get_transaction", bootstrap["transaction"])["transaction"]
    funded_tx = l1.call("get_transaction", confirmed["transaction"])["transaction"]
    before = binary(previous_tx["outputs_data"][0])
    after = binary(funded_tx["outputs_data"][0])
    assert before[:32] != after[:32]

    def timestamp(tip):
        return int.from_bytes(tip[72:80], "little")

    elapsed = timestamp(after) - timestamp(before)
    assert 0 <= elapsed < fund["policy"]["interval_ms"]
    change = l1.call("get_live_cell", confirmed["fund_change"], True)["cell"]["output"]
    assert int(change["capacity"], 16) == principal - FEE and change["lock"] == fund["lock"]

    following = publisher.publish("wallet-resumes-next-handoff", 20)
    assert following["stem_height"] == 20 and following["fee_source"] == "wallet"
    inputs = l1.call("get_transaction", following["transaction"])["transaction"]["inputs"]
    assert inputs[0]["previous_output"] == point(confirmed["transaction"], 0)
    assert inputs[1]["previous_output"] == point(bootstrap["transaction"], 1)
    assert l1.call("get_live_cell", confirmed["fund_change"], True)["status"] == "live"
    return {"publisher": publisher.report(), "fund": fund, "elapsed_stem_ms": elapsed,
            "new_stem_height": 10, "initial_funding": l1.prove_committed(donor),
            "principal_shannons": principal, "fee_shannons": FEE}

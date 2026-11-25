"""Exercise the Checkpoint Type ID Cell with live Fern and CKB test nodes."""

import base64
import json

from bridge_localnet import CKB, FEE, binary, ckb_hash, dependency, hex_bytes, le, output, point, script, script_hash, transaction
from localnet import run, write_json
from stake_localnet import committee, guardian_group, proof, witness


def capture(args, rpc, progress, minimum):
    progress(minimum)
    response = rpc.call("abci_query", path=json.dumps("/fern/stake-snapshot"), prove="true")["response"]
    if response["code"] != 0:
        raise AssertionError(response)
    height = int(response["height"])
    if height < minimum:
        raise AssertionError(response)
    stake_proof = args.output / f"checkpoint-{height}-stake.json"
    successor = args.output / f"checkpoint-{height}-successor.json"
    write_json(stake_proof, json.loads(base64.b64decode(response["value"])))
    progress(height + 1)
    write_json(successor, rpc.call("commit", height=height + 1))
    return height, stake_proof, successor


def publish(args, scenario, spec, comet_genesis, handoffs, first, second):
    l1 = scenario.l1
    fixture = scenario.fixture
    program = args.checkpoint.read_bytes()
    code_hash = hex_bytes(ckb_hash(program))
    faucet_change = point(scenario.code["out_point"]["tx_hash"], 2)
    funding_tx = l1.call("get_transaction", faucet_change["tx_hash"])["transaction"]
    funds = int(funding_tx["outputs"][2]["capacity"], 16)
    code_capacity = (len(program) + 200) * CKB
    funding_capacity = 8_000 * CKB
    deploy = transaction(
        [faucet_change],
        [output(capacity, fixture.standard)
         for capacity in (code_capacity, funding_capacity, funds - code_capacity - funding_capacity - FEE)],
        [hex_bytes(program), "0x", "0x"],
        [scenario.secp],
    )
    deploy_hash = l1.send(scenario.sign("checkpoint-deploy", deploy, 0))
    code = dependency(point(deploy_hash, 0))
    funding = point(deploy_hash, 1)
    type_id = ckb_hash(le(0, 8) + binary(deploy_hash) + le(1, 4) + le(0, 8))
    old_keys, new_keys = guardian_group(11), guardian_group(31)

    def generation(keys):
        return ckb_hash(b"Fern CKB Checkpoint authority\0" + committee(keys))

    initial = generation(old_keys)
    policy = type_id + scenario.network + scenario.genesis + initial
    type_script = script(code_hash, hex_bytes(policy), "data2")
    identity = script_hash(type_script)
    lock = script(code_hash, hex_bytes(identity + bytes(1)), "data2")
    checkpoint_capacity = 3_000 * CKB

    def out_point_bytes(out_point):
        return binary(out_point["tx_hash"]) + le(int(out_point["index"], 16), 4)

    trust = [
        "--network", hex_bytes(scenario.network), "--l1-genesis", hex_bytes(scenario.genesis),
        "--checkpoint-type", hex_bytes(identity), "--initial-generation", hex_bytes(initial),
    ]
    archive = ["--archive-db", args.output / "app-0/db", "--index", args.output / "history-index"]

    previous = None
    rejected = {}
    results = []
    for position, (height, stake_proof, successor) in enumerate((first, second)):
        relevant_handoffs = [
            item for item in handoffs if int(item["commit"]["signed_header"]["header"]["height"]) < height + 1
        ]
        transitions = args.output / f"checkpoint-{height}-handoffs.json"
        write_json(transitions, relevant_handoffs)
        prepare = [
            args.binary, "checkpoint", "prepare", *trust, *archive, "--height", str(height),
            "--genesis", spec, "--comet-genesis", comet_genesis, "--signed-successor", successor,
            "--proof-verifier", args.proof_verifier, "--stake-proof", stake_proof, "--handoffs", transitions,
        ]
        options = []
        existing = []
        if previous is not None:
            old_data, old_out = previous
            options = ["--previous-data", old_data]
            prepare += [*options, "--previous-out-point", hex_bytes(out_point_bytes(old_out)),
                        "--new-generation", hex_bytes(generation(new_keys))]
            existing = [old_out]
        plan = json.loads(run(*prepare))
        proposal = args.output / f"checkpoint-{height}-proposal.json"
        write_json(proposal, plan)
        amount = funding_capacity - (checkpoint_capacity if position == 0 else 0) - FEE
        tx = transaction(
            [*existing, funding],
            [output(checkpoint_capacity, lock, type_script), output(amount, fixture.standard)],
            [plan["data"], "0x"],
            [scenario.secp, code],
        )
        msg = binary(plan["signing_message"])
        suffix = b"".join(binary(node) for node in plan["suffix"])
        path = le(len(plan["suffix"]), 2) + suffix
        sigs = proof(old_keys, msg)
        current = path + sigs

        def signed(label, payload):
            tx["witnesses"] = [witness(payload)]
            return scenario.sign(label, tx, position)

        if position == 0:
            rejected["minority"] = l1.reject(signed("checkpoint-minority", path + proof(old_keys, msg, 2)))
        else:
            rejected["missing_rotation"] = l1.reject(signed("checkpoint-missing-rotation", current))
            replacement = proof(new_keys, msg)
            if not suffix:
                raise AssertionError("checkpoint fixtures should span a non-empty suffix")
            corrupt_path = path[:2] + bytes([path[2] ^ 1]) + path[3:]
            rejected["wrong_history"] = l1.reject(signed("checkpoint-wrong-history", corrupt_path + sigs + replacement))
            current += replacement
        update = args.output / f"checkpoint-{height}-update.json"
        write_json(update, hex_bytes(current))
        checked = json.loads(run(args.binary, "checkpoint", "verify", *trust,
                                 "--proposal", proposal, "--update", update, *options))
        if checked["height"] != height:
            raise AssertionError(checked)
        sweep_hash = l1.send(signed(f"checkpoint-publish-{height}", current))
        data = args.output / f"checkpoint-{height}-cell.json"
        write_json(data, plan["data"])
        previous = data, point(sweep_hash, 0)
        results.append({"height": height, "tx": sweep_hash, "ckb_inclusion": l1.prove_committed(sweep_hash)})
        funding_capacity = amount
        funding = point(sweep_hash, 1)

    height = second[0]
    history = json.loads(run(args.binary, "checkpoint", "history", *archive,
                             "--network", hex_bytes(scenario.network), "--height", str(height),
                             "--header", str(first[0])))
    write_json(args.output / "checkpoint-history-proof.json", history)
    return {"type": hex_bytes(identity), "publications": results, "rejections": rejected}

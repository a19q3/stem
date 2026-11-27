"""Exercise the certified write boundary on disposable weighted replicas."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import struct
import subprocess
import time

from jolteon_localnet import (
    FINALITY, HEADER, STATUS, SUBMIT, binary, covered_header, evidence_keys, genesis_and_payments,
    rpc, run, wait_for, write_configuration, write_json,
)


def scenario(args):
    args.output.mkdir()
    verifier = args.verifier or args.reference
    keys = evidence_keys(args)
    ports = [args.port + index for index in range(args.validators)]
    spec, signed = genesis_and_payments(args, keys, [1, 2, 1, 2])
    configuration = write_configuration(args.output, [key for key, _ in keys], ports, 70, 500)
    processes = {}
    target = args.target
    phase = "before-sync" if args.mode == "overlap" else args.mode
    files = {key: args.output / key.lower() for key in ("ARM", "REACHED", "RELEASE")}

    def spawn(index, initialize, fault=False):
        cmd = [
            args.binary if index == target else args.reference, "run", "--archive",
            "--home", args.output / f"node-{index}", "--configuration", configuration,
            "--genesis", spec, "--key-file", keys[index][1],
        ]
        if initialize:
            cmd.append("--initialize")
        environment = os.environ.copy()
        if fault:
            environment["LD_PRELOAD"] = str(args.shim)
            environment["STEM_COMMIT_FAULT_MODE"] = phase
            for key, path in files.items():
                environment[f"STEM_COMMIT_FAULT_{key}"] = str(path)
        with (args.output / f"node-{index}.log").open("ab") as log:
            processes[index] = subprocess.Popen(cmd, env=environment, stdout=log, stderr=subprocess.STDOUT)

    def stop(indices, crash=False):
        for index in indices:
            process = processes[index]
            if process.poll() is None:
                if crash:
                    process.kill()
                else:
                    process.terminate()
                try:
                    process.wait(4)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()

    def status(index):
        return rpc(ports[index], STATUS)

    def ready(indices, height, payments=0):
        samples = [status(index) for index in indices]
        return samples if all(s["height"] >= height and s["committed_transactions"] >= payments
                              for s in samples) else None

    def submit(batch):
        for tx in batch:
            outcome = rpc(ports[peers[0]], SUBMIT, binary(tx))
            assert "admitted" in outcome, outcome

    all_nodes = list(range(args.validators))
    peers = [index for index in all_nodes if index != target]
    paused_payments = len(signed) // 2 if args.with_payments else 0
    try:
        for index in all_nodes:
            spawn(index, True, index == target)
        wait_for(lambda: ready(all_nodes, 3), "initial finality")
        files["ARM"].touch()
        deadline = time.monotonic() + 10
        while not files["REACHED"].exists():
            assert time.monotonic() < deadline, f"writer did not reach {phase}"
            time.sleep(0.005)
        paused = status(target)
        height = paused["height"]
        count = paused["committed_transactions"]
        ack = paused["execution"]["commit_jobs"]["acknowledged"]
        assert ack == height, paused
        pending_height = struct.pack("<Q", height + 1)
        with ThreadPoolExecutor(max_workers=2) as workers:
            reads = [(method, field, workers.submit(rpc, ports[target], method, pending_height))
                     for method, field in [(FINALITY, "proof"), (HEADER, "header")]]
            submit(signed[:paused_payments])
            peer_samples = wait_for(lambda: ready(peers, height + 2, count + paused_payments),
                                    "other replicas finalise with a blocked writer")
            for _, _, request in reads:
                if request.done():
                    try:
                        premature = request.result()
                    except TimeoutError:
                        pass
                    else:
                        raise AssertionError(f"read returned before acknowledgement: {premature}")
            if args.mode == "overlap":
                files["RELEASE"].touch()
                for method, field, request in reads:
                    try:
                        response = request.result(timeout=4)
                    except TimeoutError:
                        response = rpc(ports[target], method, pending_height)
                    assert field in response, response
            else:
                stop([target], crash=True)
                stop(peers)
                for _, _, request in reads:
                    try:
                        request.result(timeout=4)
                    except (OSError, ValueError, struct.error):
                        pass
                    else:
                        raise AssertionError("certificate was visible before acknowledgement")
        if args.mode == "overlap":
            wait_for(lambda: ready(all_nodes, height + 3), "replica catches up after release")
            recovered = status(target)
        else:
            exported = json.loads(run(
                args.binary, "jolteon-snapshot", "export", "--home", args.output / f"node-{target}",
                "--genesis", spec, "--snapshot", args.output / "crash.snapshot",
                "--certificate", args.output / "crash-certificate.json",
            ))
            durable = exported["height"]
            if args.mode == "before":
                assert durable == height, exported
            elif args.mode == "after-sync":
                assert durable == height + 1, exported
            else:
                assert durable in (height, height + 1), exported
            spawn(target, False)
            recovered = wait_for(lambda: ready([target], durable), "restart with matching tip and vote journal")[0]
            actual = recovered["height"]
            finalized = rpc(ports[target], FINALITY, struct.pack("<Q", actual))
            assert "proof" in finalized, finalized
            proof = args.output / "recovered-proof.json"
            write_json(proof, finalized["proof"])
            assert json.loads(run(verifier, "jolteon-proof", "--genesis", spec, "--proof", proof)) == (
                rpc(ports[target], HEADER, struct.pack("<Q", actual)))
            for index in peers:
                spawn(index, False)

        wait_for(lambda: ready(all_nodes, height + 3), "all replicas recovered")
        submit(signed[paused_payments:])
        served = wait_for(lambda: ready(all_nodes, height + 4, count + len(signed)), "all payments finalized")
        if args.mode == "overlap" and not args.with_payments:
            assert served[target]["execution"]["commit_jobs"]["descendants"] >= 1, served[target]
            assert served[target]["execution"]["candidate_jobs"]["rebased"] >= 1, served[target]
        common = min(sample["height"] for sample in served)
        headers = [rpc(p, HEADER, struct.pack("<Q", common)) for p in ports]
        assert "header" in headers[0] and all(h == headers[0] for h in headers), headers
        proof_path = args.output / "final-proof.json"
        write_json(proof_path, rpc(ports[target], FINALITY, struct.pack("<Q", common))["proof"])
        anchor = json.loads(run(verifier, "jolteon-proof", "--genesis", spec, "--proof", proof_path))
        network = bytes.fromhex(json.loads(spec.read_text())["network_id"].removeprefix("0x"))
        certified, ancestry = covered_header(network, anchor, common,
                                             lambda h: rpc(ports[target], HEADER, struct.pack("<Q", h)))
        assert certified == headers[0], (certified, headers[0])
        summary = {
            "mode": args.mode, "paused_height": height, "recovered_height": recovered["height"],
            "durable_after_crash": None if args.mode == "overlap" else durable,
            "common_height": common, "confirmed": len(signed),
            "submitted_while_paused": paused_payments,
            "minimum_peer_confirmed_while_paused": min(s["committed_transactions"] for s in peer_samples) - count,
            "certified_common_header": certified, "certificate_ancestry": ancestry,
            "while_paused": paused["execution"]["commit_jobs"],
            "done": served[target]["execution"]["commit_jobs"],
            "candidate": served[target]["execution"]["candidate_jobs"],
        }
        write_json(args.output / "summary.json", summary)
        print(json.dumps(summary), flush=True)
    finally:
        files["RELEASE"].touch()
        stop(processes)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("binary", "reference", "lock", "shim", "output"):
        parser.add_argument(f"--{field}", type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--verifier", type=lambda p: Path(p).resolve(),
                        help="Release binary for offline proof verification, defaults to --reference")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--target", type=int, choices=(0, 2), default=0)
    parser.add_argument("--mode", choices=("before", "before-sync", "after-sync", "overlap"), required=True)
    parser.add_argument("--with-payments", action="store_true",
                        help="Confirm half the signed workload at the remaining replicas before releasing the writer")
    args = parser.parse_args()
    args.validators = 4
    args.transactions = 24
    scenario(args)


if __name__ == "__main__":
    main()

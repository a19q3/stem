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
    FINALITY, HEADER, STATUS, SUBMIT, binary, evidence_keys, genesis_and_payments,
    rpc, run, wait_for, write_configuration, write_json,
)


def scenario(args):
    args.output.mkdir()
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

    all_nodes = list(range(args.validators))
    peers = [index for index in all_nodes if index != target]
    try:
        for index in all_nodes:
            spawn(index, True, index == target)
        wait_for(lambda: ready(all_nodes, 3), "initial finality")
        files["ARM"].touch()
        wait_for(files["REACHED"].exists, f"writer reached {phase}")
        paused = status(target)
        height = paused["height"]
        count = paused["committed_transactions"]
        ack = paused["execution"]["commit_jobs"]["acknowledged"]
        pending_height = struct.pack("<Q", height + 1)
        with ThreadPoolExecutor(max_workers=2) as workers:
            proof_request = workers.submit(rpc, ports[target], FINALITY, pending_height)
            header_request = workers.submit(rpc, ports[target], HEADER, pending_height)
            wait_for(lambda: ready(peers, height + 2), "other replicas overlap blocked writer")
            during = status(target)
            assert during["height"] == height and during["committed_transactions"] == count, during
            assert during["execution"]["commit_jobs"]["acknowledged"] == ack, during
            assert not proof_request.done() and not header_request.done(), "read exposed pending commit"
            if args.mode == "overlap":
                files["RELEASE"].touch()
                assert "header" in header_request.result(timeout=4)
                assert "proof" in proof_request.result(timeout=4)
            else:
                stop([target], crash=True)
                stop(peers)
                for request in (proof_request, header_request):
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
            assert json.loads(run(args.reference, "jolteon-proof", "--genesis", spec, "--proof", proof)) == (
                rpc(ports[target], HEADER, struct.pack("<Q", actual)))
            for index in peers:
                spawn(index, False)

        wait_for(lambda: ready(all_nodes, height + 3), "all replicas recovered")
        for tx in signed:
            outcome = rpc(ports[peers[0]], SUBMIT, binary(tx))
            assert "admitted" in outcome, outcome
        served = wait_for(lambda: ready(all_nodes, height + 4, count + len(signed)), "all payments finalized")
        if args.mode == "overlap":
            assert during["execution"]["commit_jobs"]["descendants"] >= 1, during
            assert served[target]["execution"]["candidate_jobs"]["rebased"] >= 1, served[target]
        common = min(sample["height"] for sample in served)
        headers = [rpc(p, HEADER, struct.pack("<Q", common)) for p in ports]
        assert "header" in headers[0] and all(h == headers[0] for h in headers), headers
        summary = {
            "mode": args.mode, "paused_height": height, "recovered_height": recovered["height"],
            "durable_after_crash": None if args.mode == "overlap" else durable,
            "common_height": common, "confirmed": len(signed),
            "while_paused": during["execution"]["commit_jobs"],
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
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--target", type=int, choices=(0, 2), default=0)
    parser.add_argument("--mode", choices=("before", "before-sync", "after-sync", "overlap"), required=True)
    args = parser.parse_args()
    args.validators = 4
    args.transactions = 24
    scenario(args)


if __name__ == "__main__":
    main()

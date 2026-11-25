#!/usr/bin/env python3
"""Run the frozen CometBFT comparison with its installed user service templates."""

import argparse
import base64
import json
from pathlib import Path
import re
import subprocess
import time

from bridge_localnet import sidechain_tx
from localnet import Rpc, await_genesis, config_value, init_network, run, write_json
from snapshot_localnet import indexed_cell


def systemctl(*args):
    return subprocess.check_output(["systemctl", "--user", *args], text=True, timeout=40).strip()


def service_units(node, rpc_index):
    parts = re.split(r"(?m)^\[(.+)\]\n", node.config.read_text())
    for name, port in (("rpc", node.rpc), ("p2p", node.p2p)):
        index = parts.index(name) + 1
        parts[index], count = re.subn(r"(?m)^laddr = .*?$", f'laddr = "tcp://127.0.0.1:{port}"', parts[index])
        if count != 1:
            raise RuntimeError(f"Missing unique {name}.laddr in CometBFT configuration")
    node.config.write_text(parts[0] + "".join(
        f"[{parts[index]}]\n{parts[index + 1]}" for index in range(1, len(parts), 2)))
    config_value(node.config, "persistent_peers", json.dumps(node.peers))
    config_value(node.config, "pex", "false")
    config_value(node.config, "create_empty_blocks", "false")
    config_value(node.config, "create_empty_blocks_interval", json.dumps(node.args.idle_interval))
    environment = {
        "STEM_BINARY": node.args.binary,
        "COMETBFT_BINARY": node.args.comet,
        "APP_HOME": node.app,
        "STEM_GENESIS": node.args.output / "app-state.json",
        "COMET_HOME": node.home,
        "ABCI_ADDRESS": f"unix://{node.app}/abci.sock",
        "VM_WORKERS": node.args.vm_workers,
        "HISTORY_WINDOW": 256,
        "SNAPSHOT_INTERVAL": 32,
    }
    if rpc_index:
        environment["RPC_INDEX_ARGS"] = "--rpc-index"
        if node.args.admission_workers is not None:
            environment["ADMISSION_ARGS"] = f"--admission-workers={node.args.admission_workers}"

    def value(name):
        return str(environment[name]).replace("\\", "\\\\").replace('"', '\\"')

    (node.directory / "node.env").write_text("".join(
        f'{name}="{value(name)}"\n' for name in environment))
    return [run("systemd-escape", "--path", f"--template={template}", node.directory) for template in
            ("stem@.target", "stem-app@.service", "stem-comet@.service")]


def pid(unit):
    return int(systemctl("show", "--value", "--property=MainPID", unit))


def admission_workers(process):
    tasks = Path(f"/proc/{process}/task")
    return sum(path.read_text().startswith("stem-admit-") for path in tasks.glob("*/comm"))


def wait_pids(units, check, error):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        result = [pid(unit) for unit in units]
        if check(result):
            return result
        time.sleep(0.1)
    raise TimeoutError(error)


def wait_receipt(node, original):
    rpc = Rpc(node.rpc)
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            try:
                receipt = rpc.call("tx", hash="0x" + original["hash"])
            except (OSError, RuntimeError):
                rpc.close()
                time.sleep(0.1)
                continue
            if (receipt["height"], receipt["tx_result"]["code"], receipt["tx_result"]["data"]) != (
                    original["height"], 0, original["tx_result"]["data"]):
                raise RuntimeError(f"Replica {node.number} returned a different receipt")
            return
        raise TimeoutError(f"Replica {node.number} did not recover the committed payment")
    finally:
        rpc.close()


def check_wallet_cell(rpc, receipt):
    tx_hash = base64.b64decode(receipt["tx_result"]["data"])[:32].hex()
    indexed_cell(rpc, tx_hash)


def scenario(args):
    nodes, fixture = init_network(args)
    indexed = nodes[-1]
    units = [service_units(node, node is indexed) for node in nodes]
    for unit in units[0]:
        if systemctl("show", "--value", "--property=LoadState", unit) != "loaded":
            raise RuntimeError(f"Install the service templates before running the scenario: {unit}")
    targets = [target for target, _, _ in units]
    wallet_rpc = Rpc(indexed.rpc)
    try:
        systemctl("start", *targets)
        await_genesis(nodes)
        paid = sidechain_tx(nodes[0], fixture["transactions"][0])
        for node in nodes:
            wait_receipt(node, paid)
        check_wallet_cell(wallet_rpc, paid)
        wallet_rpc.close()

        target, app, comet = units[-1]
        before = [pid(app), pid(comet)]
        configured_workers = args.admission_workers or args.vm_workers
        if admission_workers(before[0]) != configured_workers:
            raise RuntimeError("Indexed validator did not start the configured admission pool")
        start = time.monotonic()
        systemctl("kill", "--kill-whom=main", "--signal=SIGKILL", app)
        wait_pids([comet], lambda pids: not any(pids), "CometBFT did not stop after ABCI crashed")
        during = sidechain_tx(nodes[0], fixture["transactions"][1])
        after = wait_pids(
            [app, comet], lambda pids: all(current and current != old for current, old in zip(pids, before)),
            "The application and consensus services did not restart")
        for node in nodes:
            wait_receipt(node, during)
        if admission_workers(after[0]) != configured_workers:
            raise RuntimeError("Recovered validator did not restore the configured admission pool")
        check_wallet_cell(wallet_rpc, paid)
        check_wallet_cell(wallet_rpc, during)
        recovery_s = time.monotonic() - start
        wallet_rpc.close()

        systemctl("stop", target)
        wait_pids([app, comet], lambda pids: not any(pids), "Stopping the target did not stop both services")
        systemctl("start", target)
        final = sidechain_tx(nodes[0], fixture["transactions"][2])
        for node in nodes:
            wait_receipt(node, final)
        check_wallet_cell(wallet_rpc, final)
        report = {
            "systemd_version": run("systemctl", "--version").splitlines()[0],
            "replicas": args.replicas,
            "before_crash": before, "after_recovery": after,
            "recovery_seconds": round(recovery_s, 3),
            "heights": [int(tx["height"]) for tx in (paid, during, final)],
            "receipts_on_all_replicas": [tx["hash"] for tx in (paid, during, final)],
            "wallet_index_replica": indexed.number,
            "wallet_index_verified_after_restarts": True,
            "admission_workers_before_and_after_crash": configured_workers,
        }
        write_json(args.output / "systemd-run.json", report)
        print((args.output / "systemd-run.json").read_text(), end="")
    finally:
        wallet_rpc.close()
        systemctl("stop", *[unit for replica in units for unit in replica])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("binary", "comet", "output"):
        parser.add_argument("--" + flag, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--base-port", type=int, default=31400)
    parser.add_argument("--admission-workers", type=int)
    args = parser.parse_args()
    args.replicas, args.vm_workers, args.commit_ms = 4, 2, 200
    args.idle_interval, args.rate, args.seconds = "2s", 3, 1
    args.output.mkdir(mode=0o700)
    scenario(args)


if __name__ == "__main__":
    main()

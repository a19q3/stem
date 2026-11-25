#!/usr/bin/env python3
"""Run four sidechain replicas in disposable Linux KVM guests on one physical host."""

import argparse
import lzma
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import time

from localnet import Rpc, await_genesis, config_value, init_network, process_resources, run, write_json


def install_binary(source, destination, root):
    shutil.copy2(source, destination)
    destination.chmod(destination.stat().st_mode & 0o777)
    linked = subprocess.run(["ldd", source], capture_output=True, text=True)
    if linked.returncode and not re.search("not a dynamic executable|statically linked", linked.stdout + linked.stderr):
        raise RuntimeError(linked.stderr)
    if "not found" in linked.stdout:
        raise RuntimeError(f"Missing guest dependency for {source}: {linked.stdout}")
    for path in re.findall(r"(?:=>\s+|^\s*)(/\S+)\s+\(", linked.stdout, flags=re.MULTILINE):
        target = root / path.lstrip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)


def netem_options(args):
    options = []
    for name, value, unit in (
        ("delay", args.p2p_delay_ms, "ms"),
        ("loss", args.p2p_loss_percent, "%"),
        ("duplicate", args.p2p_duplicate_percent, "%"),
        ("reorder", args.p2p_reorder_percent, "%"),
    ):
        if value:
            options.extend((name, f"{value}{unit}"))
    return options


def guest_root(args):
    root = args.output / "guest-files"
    for name in ("bin", "dev", "proc", "sys", "tmp", "run", "etc", "usr", "modules"):
        (root / name).mkdir(parents=True, exist_ok=True)
    (root / "tmp").chmod(0o1777)
    programs = ("bash", "mount", "ip", "insmod", "sleep", "uname")
    impaired = bool(netem_options(args))
    if impaired:
        programs += ("tc",)
    for name in programs:
        command = shutil.which(name)
        if command is None:
            raise RuntimeError(f"Missing host command {name}")
        install_binary(command, root / "bin" / name, root)
    install_binary(args.binary, root / "bin/fern", root)
    install_binary(args.comet, root / "bin/cometbft", root)
    modules = ["virtio_net"]
    if impaired:
        modules.append("sch_netem")
    loads = []
    installed = set()
    for module in modules:
        for line in run("modprobe", "--show-depends", module).splitlines():
            command, *fields = shlex.split(line)
            if command != "insmod" or fields[0] in installed:
                continue
            source = Path(fields[0])
            suffix = source.suffix
            path = root / "modules" / (source.stem if suffix in (".xz", ".zst", ".gz") else source.name)
            if suffix == ".xz":
                path.write_bytes(lzma.decompress(source.read_bytes()))
            elif suffix in (".gz", ".zst"):
                utility = {".gz": "gzip", ".zst": "zstd"}[suffix]
                path.write_bytes(subprocess.check_output([utility, "-dc", source]))
            else:
                shutil.copy2(source, path)
            installed.add(fields[0])
            loads.append(f"insmod /modules/{path.name}")
    return root, "\n".join(loads)


def boot(args, nodes, directory, modules, node):
    number = node.number
    address = f"192.168.232.{10 + number}"
    peers = ",".join(f"{peer.address}@192.168.232.{10 + peer.number}:26656" for peer in nodes if peer is not node)
    config_value(node.config, "prometheus_listen_addr", '"10.0.2.15:26660"')
    comet = directory / "comet"
    comet.mkdir()
    shutil.copytree(node.home / "config", comet / "config")
    shutil.copytree(node.home / "data", comet / "data")
    shutil.copy2(args.output / "app-state.json", directory / "app-state.json")
    if args.proposal_timeout_ms is not None:
        config_value(comet / "config/config.toml", "timeout_propose", f'"{args.proposal_timeout_ms}ms"')
    netem = netem_options(args)
    configure_p2p = ""
    sample_p2p = ""
    if netem:
        if args.p2p_netem_seed is not None:
            netem.extend(("seed", str(args.p2p_netem_seed + number)))
        configure_p2p = f"tc qdisc add dev eth1 root netem {shlex.join(netem)}\ntc qdisc show dev eth1"
        sample_p2p = r"""(
  while sleep 5; do
    stats=$(tc -s -j qdisc show dev eth1)
    printf 'sidechain_p2p_qdisc=%s\n' "$stats"
  done
) &"""
    queue = f" --abci-response-queue {args.abci_response_queue}" if args.abci_response_queue is not None else ""
    init = directory / "init"
    init.write_text(f"""#!/bin/bash
set -e
export PATH=/bin
umask 077
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /run
{modules}
ip link set lo up
ip addr add 10.0.2.15/24 dev eth0
ip link set eth0 up
ip addr add {address}/24 dev eth1
ip link set eth1 up
{configure_p2p}
uname -r
ip -br addr
fern run --home /app --genesis /app-state.json --workers {args.vm_workers} \\
  --admission-workers {args.admission_workers}{queue} --compact-transaction-history --retain-consensus-blocks 96 &
app_pid=$!
until [[ -S /app/abci.sock ]]; do
    kill -0 "$app_pid"
    sleep 0.1
done
cometbft start --home /comet --proxy_app unix:///app/abci.sock --rpc.laddr tcp://10.0.2.15:26657 \\
  --p2p.laddr tcp://{address}:26656 --p2p.persistent_peers {shlex.quote(peers)} --p2p.pex=false \\
  --consensus.create_empty_blocks=false --consensus.create_empty_blocks_interval=2s &
comet_pid=$!
{sample_p2p}
wait -n "$app_pid" "$comet_pid"
""")
    init.chmod(0o755)
    image = node.directory / "guest.ext4"
    with image.open("xb") as stream:
        stream.truncate(2 << 30)
    run("mkfs.ext4", "-Fq", "-d", directory, image)
    command = [args.qemu, "-enable-kvm", "-cpu", "host", "-m", str(args.vm_memory_mib), "-smp", str(args.vm_cpus),
               "-display", "none", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-kernel", args.kernel,
               "-append", "root=/dev/vda rw rootfstype=ext4 init=/init console=ttyS0 net.ifnames=0 quiet panic=1",
               "-drive", f"file={image},format=raw,if=virtio,cache=none",
               "-netdev", f"user,id=rpc,restrict=on,hostfwd=tcp:127.0.0.1:{node.rpc}-:26657,hostfwd=tcp:127.0.0.1:{node.metrics}-:26660",
               "-device", f"virtio-net-pci,netdev=rpc,mac=52:54:00:12:34:{number:02x}",
               "-netdev", f"socket,id=peers,mcast=239.255.79.31:{args.base_port + 200},localaddr=127.0.0.1",
               "-device", f"virtio-net-pci,netdev=peers,mac=52:54:00:34:56:{number:02x}"]
    write_json(node.directory / "launch.json", [str(value) for value in command])
    with (node.directory / "console.log").open("wb") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
    node.processes.append(process)
    return image


def vm_sample(nodes, images):
    return [dict(process_resources(node.processes[0].pid), disk_allocated_bytes=image.stat().st_blocks * 512)
            for node, image in zip(nodes, images)]


def peer_connections(nodes):
    deadline = time.monotonic() + 15
    observations = []
    for node in nodes:
        expected = {peer.address for peer in nodes if peer is not node}
        rpc = Rpc(node.rpc, timeout=2)
        try:
            while time.monotonic() < deadline:
                peers = rpc.call("net_info")["peers"]
                if {peer["node_info"]["id"] for peer in peers} == expected:
                    observations.append([{"id": peer["node_info"]["id"], "guest_ip": peer["remote_ip"]}
                                         for peer in peers])
                    break
                time.sleep(0.1)
            else:
                raise TimeoutError(f"Guest {node.number} did not connect to its three peers")
        finally:
            rpc.close()
    return observations


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "comet", "loadgen", "proof-verifier", "output"):
        parser.add_argument("--" + name, type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--kernel", type=Path, default=Path(f"/boot/vmlinuz-{platform.release()}"))
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    parser.add_argument("--base-port", type=int, default=34600)
    parser.add_argument("--vm-cpus", type=int, default=3)
    parser.add_argument("--vm-memory-mib", type=int, default=2048)
    parser.add_argument("--vm-workers", type=int, default=2)
    parser.add_argument("--admission-workers", type=int, default=3)
    parser.add_argument("--abci-response-queue", type=int, help="queued responses per guest ABCI connection")
    parser.add_argument("--proposal-timeout-ms", type=int, help="initial CometBFT proposal timeout in each guest")
    parser.add_argument("--p2p-delay-ms", type=int, default=0)
    parser.add_argument("--p2p-loss-percent", type=float, default=0)
    parser.add_argument("--p2p-duplicate-percent", type=float, default=0)
    parser.add_argument("--p2p-reorder-percent", type=float, default=0)
    parser.add_argument("--p2p-netem-seed", type=int, help="base seed incremented for each guest")
    parser.add_argument("--rate", type=int, default=300)
    parser.add_argument("--seconds", type=int, default=60)
    args = parser.parse_args()
    if args.abci_response_queue is not None and not 1 <= args.abci_response_queue <= 128:
        parser.error("ABCI response queue must be between 1 and 128")
    if args.proposal_timeout_ms is not None and args.proposal_timeout_ms < 1:
        parser.error("The initial proposal timeout must be positive")
    probabilities = args.p2p_loss_percent, args.p2p_duplicate_percent, args.p2p_reorder_percent
    if not all(0 <= value < 100 for value in probabilities):
        parser.error("P2P impairment probabilities must be between 0 and 100 percent, excluding 100")
    if args.p2p_reorder_percent and args.p2p_delay_ms <= 0:
        parser.error("P2P reordering requires a positive P2P delay")
    if args.p2p_netem_seed is not None and (not netem_options(args) or
                                           not 1 <= args.p2p_netem_seed <= (1 << 64) - 4):
        parser.error("The P2P seed requires netem and room for four positive guest seeds")
    if (not 1 <= args.rate * args.seconds <= 262_144 or min(args.rate, args.seconds, args.vm_cpus) < 1 or
            args.vm_memory_mib < 1024 or not 1 <= args.base_port <= 65_335 or not 0 <= args.p2p_delay_ms <= 500 or
            not 1 <= args.vm_workers <= 64 or not 1 <= args.admission_workers <= 64):
        parser.error("Invalid guest resources, port, delay or payment count")
    args.replicas, args.commit_ms, args.idle_interval = 4, 200, "2s"
    args.prefund_payments = True
    args.checkpoint_interval = 24
    args.checkpoint_cell_proofs = args.checkpoint_compact_transactions = args.checkpoint_live_cell_replay = True
    args.no_comet_tx_index = True
    args.output.mkdir(mode=0o700)
    nodes, _ = init_network(args)
    root, modules = guest_root(args)
    topology = args.output / "topology.json"
    write_json(topology, {"placement": "virtualized", "replicas": [
        {"label": f"vm-{node.number}", "host": "shared-physical-host", "rpc": f"http://127.0.0.1:{node.rpc}",
         "metrics": f"http://127.0.0.1:{node.metrics}/metrics"} for node in nodes],
        "p2p_delay_ms": args.p2p_delay_ms, "guest_cpus": args.vm_cpus, "guest_memory_mib": args.vm_memory_mib,
        "p2p_netem": {"loss_percent": args.p2p_loss_percent, "duplicate_percent": args.p2p_duplicate_percent,
                      "reorder_percent": args.p2p_reorder_percent, "base_seed": args.p2p_netem_seed},
        "admission_workers": args.admission_workers, "abci_response_queue": args.abci_response_queue,
        "proposal_timeout_ms": args.proposal_timeout_ms or 1000,
        "guest_kernel": args.kernel.name, "qemu": run(args.qemu, "--version").splitlines()[0]})
    images = []
    driver = None
    try:
        for node in nodes:
            # Each image is built from a common system root and its own signing and P2P keys.
            installed = node.directory / "root-files"
            shutil.copytree(root, installed, copy_function=shutil.copy2)
            images.append(boot(args, nodes, installed, modules, node))
        await_genesis(nodes)
        write_json(args.output / "guest-peers.json", peer_connections(nodes))
        output = args.output / "benchmark"
        command = [sys.executable, Path(__file__).with_name("network_bench.py"), "--binary", args.binary,
                   "--loadgen", args.loadgen, "--proof-verifier", args.proof_verifier, "--topology", topology,
                   "--workload", args.output / "workload.json", "--genesis", nodes[0].home / "config/genesis.json",
                   "--rate", str(args.rate), "--prefund-payments", "--output", output]
        write_json(args.output / "benchmark-command.json", [str(value) for value in command])
        driver = subprocess.Popen(command)
        samples = []
        while driver.poll() is None:
            samples.append({"unix_ns": time.time_ns(), "guests": vm_sample(nodes, images)})
            time.sleep(1)
        samples.append({"unix_ns": time.time_ns(), "guests": vm_sample(nodes, images)})
        write_json(args.output / "guest-resources.json", samples)
        if driver.returncode:
            raise RuntimeError(f"External benchmark exited with status {driver.returncode}")
    finally:
        if driver is not None and driver.poll() is None:
            driver.terminate()
            driver.wait(timeout=10)
        for node in nodes:
            for process in node.processes:
                if process.poll() is None:
                    process.terminate()
        for node in nodes:
            for process in node.processes:
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    main()

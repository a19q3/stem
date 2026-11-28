"""Regression cases for local state-engine resource evidence."""

import ipaddress
import json
from pathlib import Path
import sys
import tempfile
import unittest

if __package__:
    from .compare_state import check_reported_resources
    from .runtime import finish_runtime, proc_tcp_address, read_runtime, run_arguments
else:
    from compare_state import check_reported_resources
    from runtime import finish_runtime, proc_tcp_address, read_runtime, run_arguments


class RuntimeEvidenceTests(unittest.TestCase):
    def test_linux_loopback_addresses_and_argument_forms(self):
        def encode(address):
            raw = ipaddress.ip_address(address).packed
            return "".join(f"{int.from_bytes(raw[i:i + 4], sys.byteorder):08X}"
                           for i in range(0, len(raw), 4)) + ":1234"

        for address in ("127.0.0.1", "::1"):
            self.assertEqual(proc_tcp_address(encode(address)), (ipaddress.ip_address(address), 4660))
        self.assertEqual(proc_tcp_address(encode("::ffff:127.0.0.1")), (ipaddress.ip_address("127.0.0.1"), 4660))
        args = ["stem", "run", "--home", "/node", "--configuration=/peers",
                "--genesis", "/genesis", "--key-file", "/key", "--archive"]
        self.assertTrue(run_arguments(args)["archive"])
        with self.assertRaises(ValueError):
            run_arguments(args + ["--configuration", "/other-peers"])

    def test_runtime_evidence_binds_report_and_rejects_restarted_process(self):
        row = {
            "endpoint": "127.0.0.1:8000", "pid": 6, "start_ticks": 10,
            "configuration": {"binary_sha256": "fixture", "cpu_affinity": [0, 1]},
            "cpu_ticks": 11, "peak_rss_kib": 1000, "read_bytes": 8, "write_bytes": 32,
        }
        run = {"cpu_ticks_per_second": 100, "driver_cpu_affinity": [2], "replicas": [row]}
        after = {**run, "replicas": [{**row, "cpu_ticks": 44, "peak_rss_kib": 1100}]}
        endpoint = (ipaddress.ip_address("127.0.0.1"), 8000)
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            runtime = Path(directory) / "runtime.json"
            report.write_text("{}")
            runtime.write_text(json.dumps(finish_runtime(run, after, report)))
            self.assertEqual(read_runtime(runtime, report, [endpoint])["replicas"][0]["process"]["cpu_ticks"], 33)
            self.assertNotIn("pid", json.loads(runtime.read_text())["replicas"][0])
            with self.assertRaises(ValueError):
                finish_runtime(run, {**run, "replicas": [{**row, "start_ticks": 31}]}, report)
            report.write_text('{"different":true}')
            with self.assertRaises(ValueError):
                read_runtime(runtime, report, [endpoint])

    def test_reported_index_budget_is_checked_but_residency_is_an_outcome(self):
        config = {
            "history_window": None, "live_cell_index_mib": 8, "cell_node_index_mib": None,
            "storage_metrics": True, "live_cell_recovery_workers": 2,
            "live_cell_index_intern_scripts": True,
        }
        runtime = {"replicas": [{"endpoint": "127.0.0.1:8000", "configuration": config}]}
        live = {"limit_bytes": 8 << 20, "layout": "interned_scripts", "recovery_workers": 2, "active": True}
        cache = {"capacity": 32, "shards": 1, "lock_sample_interval": 0}
        before = {"history_window": None, "live_cell_index": live, "cell_node_index": None, "verification_cache": cache}
        after = {**before, "live_cell_index": {**live, "active": False}}
        observation = {"load": {"measured": True}, "reported_node_resources": {"before": [before], "after": [after]}}
        self.assertEqual(check_reported_resources(runtime, observation), [cache])
        after["live_cell_index"]["limit_bytes"] //= 2
        with self.assertRaises(ValueError):
            check_reported_resources(runtime, observation)


if __name__ == "__main__":
    unittest.main()

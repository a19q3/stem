import unittest

from qualify import MINUTE_US, counts_in_windows, host_inventory, score


SETTINGS = {"REPLICAS": 4, "PAYMENT_DATA_BYTES": 0, "PREFUND_PAYMENTS": 1, "RATE": 1}


class Qualification(unittest.TestCase):
    @staticmethod
    def sustainable(report, *, settings=SETTINGS, max_p99_ms=5000):
        return score(report, settings, mode="sustainable", target=1, warmup_seconds=60,
                     max_unacknowledged_bps=10, max_pending=5, max_pending_growth=1, max_p99_ms=max_p99_ms)

    def report(self):
        times = [MINUTE_US * minute + 1_000_000 * second
                 for minute in range(1, 4) for second in range(1, 61)]
        return {
            "load": {"seconds": 240, "drain": 30, "replicas": 4, "payment_chains": 0,
                     "payment_data_bytes": 0, "prefund_payments": True},
            "delivery_times": [[when - 2, when - 1] for when in times],
            "blocks": [{"seen_us": when, "positions": [i]} for i, when in enumerate(times)],
            "summary": {"planned": 180, "offered": 180, "admitted": 180, "client_drops": 0,
                        "transport_write_failures": 0, "visible_without_ack": 0},
            "observation_errors": [], "workload_id": "synthetic", "agreed_height": 180,
        }

    def test_right_aligned_windows_do_not_double_count_boundaries(self):
        first_end = 2 * MINUTE_US
        self.assertEqual(counts_in_windows([MINUTE_US, first_end, first_end + 1, None], 60, 2), [1, 1])

    def test_pass_requires_every_minute(self):
        report = self.report()
        self.assertTrue(self.sustainable(report)["passed"])
        report["blocks"][58]["seen_us"] = 120_000_001
        report["blocks"][59]["seen_us"] = 120_000_002
        result = self.sustainable(report)
        self.assertEqual(result["common_per_minute"], [58, 62, 60])
        self.assertEqual(result["acknowledged_pending_at_measurement_start_and_minute_ends"], [0, 2, 0, 0])
        self.assertFalse(result["passed"])

    def test_confirmation_after_cutoff_does_not_clear_drain(self):
        report = self.report()
        report["blocks"][-1]["seen_us"] = 270_000_001
        result = self.sustainable(report)
        self.assertEqual(result["acknowledged_pending_at_drain"], 1)
        self.assertEqual(result["write_to_common_samples"], 179)
        self.assertFalse(result["passed"])

    def test_overload_cannot_pass_sustainable_and_drained_growth_still_fails(self):
        report = self.report()
        report["delivery_times"] = [
            (second * 1_000_000 - 2, second * 1_000_000 - 1)
            for second in range(61, 241) for _ in range(3300)
        ]
        report["blocks"] = [
            {"seen_us": (60 + second) * 1_000_000,
             "positions": range(second * 3000 - 3000, second * 3000)}
            for second in range(1, 199)
        ]
        report["summary"].update(planned=594_000, offered=594_000, admitted=594_000)
        settings = {**SETTINGS, "RATE": 3300}
        overload = score(report, settings, mode="overload", target=3000, warmup_seconds=60,
                         max_unacknowledged_bps=1000, max_pending=15_000)
        self.assertEqual(overload["common_per_minute"], [180_000] * 3)
        self.assertEqual(overload["acknowledged_pending_at_measurement_start_and_minute_ends"],
                         [0, 18_000, 36_000, 54_000])
        self.assertGreater(overload["acknowledged_pending_peak"], 54_000)
        self.assertEqual(overload["acknowledged_pending_at_drain"], 0)
        self.assertFalse(overload["passed"])
        self.assertIn("peak acknowledged backlog exceeded its limit", overload["reasons"])

        sustainable = score(report, settings, mode="sustainable", target=3000, warmup_seconds=60,
                            max_unacknowledged_bps=10, max_pending=60_000, max_pending_growth=3_000,
                            max_p99_ms=None)
        self.assertIn("sustainable offer rate must equal the target", sustainable["reasons"])
        self.assertIn("acknowledged backlog grew beyond its limit across minute boundaries",
                      sustainable["reasons"])

    def test_overload_allows_bounded_refusals(self):
        report = self.report()
        report["delivery_times"].extend((120_000_000, None) for _ in range(60))
        result = score(report, {**SETTINGS, "RATE": 2}, mode="overload", target=1, warmup_seconds=60,
                       max_unacknowledged_bps=5000, max_pending=5)
        self.assertEqual(result["measurement_writes_without_ack"], 60)
        self.assertIn("actual writes did not sustain the prepared offer", result["reasons"])
        report["delivery_times"][180:] = [
            (when - 2, None) for when, _ in report["delivery_times"][:180]
        ]
        report["summary"].update(planned=360, offered=360)
        result = score(report, {**SETTINGS, "RATE": 2}, mode="overload", target=1, warmup_seconds=60,
                       max_unacknowledged_bps=5000, max_pending=5)
        self.assertTrue(result["passed"], result["reasons"])
        self.assertEqual(result["measurement_writes_without_ack"], 180)

    def test_sustainable_rejects_too_many_missing_acks_and_reports_p99(self):
        report = self.report()
        report["delivery_times"][0][1] = None
        result = self.sustainable(report)
        self.assertEqual(result["write_to_common_p99_us"], 2)
        self.assertIn("measurement write-without-ack rate exceeded its limit", result["reasons"])

    def test_sustainable_applies_its_p99_limit(self):
        result = self.sustainable(self.report(), max_p99_ms=0)
        self.assertEqual(result["write_to_common_p99_us"], 2)
        self.assertIn("measurement p99 exceeded its limit or was unavailable", result["reasons"])

    def test_shared_physical_validator_or_loopback_fails(self):
        endpoints = [f"10.0.0.{i}:28890" for i in range(1, 5)]
        inventory = {"driver": {"physical_host": "driver"}, "validators": [
            {"endpoint": endpoint, "physical_host": str(i)} for i, endpoint in enumerate(endpoints)
        ]}
        peers = [{"address": endpoint} for endpoint in endpoints]
        host_inventory(inventory, endpoints, peers)
        inventory["driver"]["physical_host"] = "1"
        with self.assertRaisesRegex(ValueError, "separate driver"):
            host_inventory(inventory, endpoints, peers)
        inventory["driver"]["physical_host"] = "driver"
        with self.assertRaisesRegex(ValueError, "prepared peers"):
            host_inventory(inventory, endpoints, peers[::-1])
        inventory["validators"][0]["endpoint"] = peers[0]["address"] = endpoints[0] = "[::ffff:127.0.0.1]:28890"
        with self.assertRaisesRegex(ValueError, "remote address"):
            host_inventory(inventory, endpoints, peers)


if __name__ == "__main__":
    unittest.main()

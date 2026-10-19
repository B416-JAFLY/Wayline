"""Hardware-free checks for the optional MSG_STATS BA/UP reply boundaries."""
import struct
import unittest

from radio_probe import decode_aggregation_stats, decode_uplink_stats


class AggregationStats(unittest.TestCase):
    def test_uplink_reply_preserves_timings_and_rejects_unknown_bounds(self):
        values = [1, 20, 5, 20000, 100, 200, 70, 10, 0, 8, 4096, 80000, 60000, 1]
        result = decode_uplink_stats(struct.pack("<14I", *values))
        self.assertEqual(result["frames"], 20)
        self.assertEqual(result["write_us"], 200)
        self.assertEqual(result["link_core"], 1)
        for body in (b"", bytes(28), bytes(55), bytes(57)):
            with self.assertRaises(ValueError):
                decode_uplink_stats(body)
        for index, value in ((0, 2), (9, 9), (10, 4097), (13, 2)):
            invalid = values.copy()
            invalid[index] = value
            with self.assertRaises(ValueError):
                decode_uplink_stats(struct.pack("<14I", *invalid))

    def test_active_and_inactive_status(self):
        active = [1, 1, 1, 5, 16, 2, 2, 0, 0, 0, 1234, 4, 0, 1]
        decoded = decode_aggregation_stats(struct.pack("<14I", *active))
        self.assertEqual(decoded["ampdu_mpdu"], 1234)
        self.assertEqual(decoded["ampdu_rx_drop"], 4)
        self.assertEqual(decoded["tx_report_drop"], 1)
        inactive = [1, 0, 0, 255, 0] + [0] * 9
        self.assertEqual(decode_aggregation_stats(struct.pack("<14I", *inactive))["active"], 0)

    def test_old_firmware_truncation_version_and_invalid_state(self):
        for body in (b"", bytes(28), bytes(55), bytes(57)):
            with self.assertRaises(ValueError):
                decode_aggregation_stats(body)
        valid = [1, 1, 1, 5, 16] + [0] * 9
        for index, value in ((0, 2), (1, 2), (2, 2), (1, 0), (3, 8), (4, 0), (4, 17), (2, 0)):
            invalid = valid.copy()
            invalid[index] = value
            with self.subTest(index=index, value=value), self.assertRaises(ValueError):
                decode_aggregation_stats(struct.pack("<14I", *invalid))


if __name__ == "__main__":
    unittest.main()

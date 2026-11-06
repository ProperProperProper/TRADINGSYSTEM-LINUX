import unittest

from papercompmoomoo import gating

NAS = frozenset({"US.TEM", "US.RIVN", "US.IDXX"})


class GatingTest(unittest.TestCase):
    def test_tier1_hygiene(self):
        self.assertIsNone(gating.tier1_hygiene("US.TEM", 81.37, NAS, set(), set()))
        self.assertIn("Nasdaq", gating.tier1_hygiene("US.LSRCY", 50.3, NAS, set(), set()))
        self.assertIn("outside", gating.tier1_hygiene("US.TEM", 1.5, NAS, set(), set()))
        self.assertIn("outside", gating.tier1_hygiene("US.TEM", 1200.0, NAS, set(), set()))
        self.assertEqual(gating.tier1_hygiene("US.TEM", 80, NAS, {"US.TEM"}, set()), "already held")
        self.assertIn("working", gating.tier1_hygiene("US.TEM", 80, NAS, set(), {"US.TEM"}))

    def test_tier2_corridor_and_rvol(self):
        self.assertIsNone(gating.tier2_inflection(81.0, 12.0, 2.0, False))
        self.assertIn("extended", gating.tier2_inflection(50.0, 80.0, 3.0, False))
        self.assertIn("RVOL", gating.tier2_inflection(50.0, 10.0, 1.5, False))
        self.assertIsNone(gating.tier2_inflection(50.0, 10.0, 1.5, True))  # pre-market threshold 1.2
        self.assertAlmostEqual(gating.base_support(110.0, 10.0), 100.0)

    def test_kinematic_acceleration(self):
        self.assertTrue(gating.accelerating([10, 10.1, 10.25, 10.45]))
        self.assertFalse(gating.accelerating([10.45, 10.3, 10.1, 9.9]))
        self.assertTrue(gating.accelerating([10, 10.1]))

    def test_tier3_has_no_artificial_margin(self):
        self.assertIsNone(gating.tier3_connectome(1.28, 0.67))
        self.assertIsNotNone(gating.tier3_connectome(0.6, 0.67))
        self.assertEqual(gating.ENTRY_MARGIN, 0.0)

    def test_tier4_sizing_and_ticks(self):
        self.assertAlmostEqual(gating.tier4_notional(99_122.5, 99_122.5, 15), 99_122.5 / 15)
        self.assertEqual(gating.tier4_notional(5_000, 99_122.5, 15), 1000.0)  # floor
        self.assertAlmostEqual(gating.tier4_notional(99_122.5, 99_122.5, 1, 5.0), 99_122.5 * 0.25)  # cap
        self.assertEqual(gating.tick(13.3549), 13.35)
        self.assertEqual(gating.tick(13.355), 13.36)
        self.assertEqual(gating.slip_limit(100.0), 100.5)


if __name__ == "__main__":
    unittest.main()

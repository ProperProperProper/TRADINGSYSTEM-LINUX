import unittest

import pandas as pd

from papercompmoomoo.signals import size_order, symbol_profile


def profile(last=1_000_000.0, typical=1_000_000.0, rng=0.005):
    return {"last_bar_volume": last, "typical_bar_volume": typical, "median_range": rng}


class Sizing(unittest.TestCase):
    def size(self, **k):
        args = dict(price=10.0, notional=100_000.0, broker_max=50_000.0, profile=profile(), avg_daily_volume=100_000_000.0, equity=1_000_000.0, stop_loss_pct=2.0)
        args.update(k)
        return size_order(**args)

    def test_the_smallest_limit_wins_and_is_named(self):
        self.assertEqual(self.size(), (10_000, "money"))
        self.assertEqual(self.size(broker_max=700.0), (700, "broker maximum"))
        self.assertEqual(self.size(profile=profile(last=5_000.0, typical=5_000.0)), (500, "this symbol's bar volume"))
        self.assertEqual(self.size(avg_daily_volume=100_000.0), (2_000, "this symbol's average daily volume"))

    def test_a_one_off_spike_in_the_last_bar_does_not_inflate_the_size(self):
        spike = self.size(profile=profile(last=60_000.0, typical=4_000.0), avg_daily_volume=1e9)
        self.assertEqual(spike, (400, "this symbol's bar volume"), "sized from the normal bar at this hour, not the spike")

    def test_a_volatile_symbol_gets_a_smaller_position_than_a_calm_one_case_by_case(self):
        calm = self.size(notional=1e9, broker_max=1e9, profile=profile(rng=0.004))
        wild = self.size(notional=1e9, broker_max=1e9, profile=profile(rng=0.04))
        self.assertEqual(calm[1], wild[1])
        self.assertIn("risk", calm[1])
        self.assertEqual(calm[0], 25_000, "0.5% of 1M over a 2% stop, at $10")
        self.assertEqual(wild[0], 12_500, "the stop cannot sit inside a 4% bar: half the calm size")

    def test_the_penny_stock_case_that_gave_640760_shares_is_now_a_small_order(self):
        shares, limit = self.size(price=0.5, notional=400_000.0, broker_max=1_466_237.0, profile=profile(last=5e6, typical=5e6, rng=0.03), avg_daily_volume=319_920.0)
        self.assertLess(shares, 7_000)
        self.assertIn(limit, ("this symbol's average daily volume", "risk (loss budget over this symbol's stop distance)"))

    def test_zero_or_unknown_means_no_order(self):
        self.assertEqual(self.size(profile=profile(last=0.0, typical=0.0))[0], 0)
        self.assertEqual(self.size(broker_max=0.0)[0], 0)
        self.assertEqual(self.size(price=0.0)[0], 0)
        self.assertEqual(self.size(equity=0.0)[0], 0)


class Profile(unittest.TestCase):
    def test_it_reads_the_symbols_own_bars(self):
        days = ["2026-10-14", "2026-10-15", "2026-10-16", "2026-10-19"]
        frames = []
        for d, volume in zip(days, (100, 200, 300, 999)):
            index = pd.date_range(f"{d} 09:30", periods=3, freq="1min", tz="America/New_York")
            frames.append(pd.DataFrame({"High": 10.2, "Low": 9.8, "Close": 10.0, "Volume": volume}, index=index))
        p = symbol_profile(pd.concat(frames))
        self.assertEqual(p["last_bar_volume"], 999.0)
        self.assertEqual(p["typical_bar_volume"], 200.0, "median of the same bar on the earlier days")
        self.assertAlmostEqual(p["median_range"], 0.04)
        self.assertEqual(symbol_profile(None), {"last_bar_volume": 0.0, "typical_bar_volume": 0.0, "median_range": 0.0})


if __name__ == "__main__":
    unittest.main()

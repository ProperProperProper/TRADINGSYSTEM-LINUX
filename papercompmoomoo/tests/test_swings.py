import unittest

from papercompmoomoo import swings


def series(closes):
    highs = [c * 1.002 for c in closes]
    lows = [c * 0.998 for c in closes]
    return highs, lows, closes


class Swings(unittest.TestCase):
    def test_a_confirmed_swing_is_entered_on_the_confirming_close_and_exited_by_the_trailing_stop(self):
        # flat at 100, then up 2.5% (confirm 2% reached at 102.5), runs to 110, then falls 3%+ from the peak
        closes = [100, 100, 100, 100.5, 102.5, 105, 108, 110, 107.0, 106.0, 105.0]
        highs, lows, closes = series(closes)
        ends = [False] * (len(closes) - 1) + [True]
        trades = swings.trade_swings(highs, lows, closes, ends, confirm=2.0, trail=3.0)
        self.assertEqual(len(trades), 1)
        net, run_up, held = trades[0]
        entry = 102.5
        peak = 110 * 1.002
        self.assertAlmostEqual(net, (peak * 0.97 / entry - 1) * 100 - swings.COSTS_PCT, places=6)
        self.assertGreater(run_up, 7.0)
        self.assertEqual(held, 4, "four bars after the entry bar, the last one breaks the trailing stop")

    def test_the_hard_stop_wins_a_bar_that_touches_both_and_a_flat_market_makes_no_entry(self):
        highs, lows, closes = series([100, 100, 100, 102.5, 100.0, 99.0, 98.0, 97.0])
        ends = [False] * 7 + [True]
        net = swings.trade_swings(highs, lows, closes, ends, confirm=2.0, trail=3.0)[0][0]
        self.assertLess(net, -2.0, "stopped out near -2.5% plus costs")
        flat = series([100.0] * 30)
        self.assertEqual(swings.trade_swings(*flat, [False] * 29 + [True], confirm=2.0, trail=3.0), [])

    def test_an_entry_never_uses_a_later_bar_and_a_session_end_closes_the_trade(self):
        closes = [100, 100, 102.5, 103, 103.5, 104, 104.5]
        highs, lows, closes = series(closes)
        ends = [False, False, False, False, True, False, False]
        trades = swings.trade_swings(highs, lows, closes, ends, confirm=2.0, trail=4.0)
        self.assertEqual(trades[0][2], 2, "closed at the end of the session (bar 4), two bars after entry")
        # changing a bar after the exit cannot change the first trade
        changed = list(closes)
        changed[6] = 50.0
        h2, l2, c2 = series(changed)
        self.assertEqual(swings.trade_swings(h2, l2, c2, ends, confirm=2.0, trail=4.0)[0], trades[0])


if __name__ == "__main__":
    unittest.main()

"""Deterministic fixtures only; no broker connection or production candle writes."""
import math
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

import pandas as pd

from papercompmoomoo import microsense, gating
from papercompmoomoo.bot import MomentumPaperBot, Settings
from papercompmoomoo.candlestore import CandleStore
from papercompmoomoo.moomoo_feed import MoomooFeed, bars_from_kline
from papercompmoomoo.signals import nasdaq_listed_symbols


class MicroSense(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "common1m.sqlite3"
        self.index = pd.date_range("2026-11-02 10:00", periods=26, freq="min", tz="America/New_York")
        self.frame = pd.DataFrame({"Open": 10.0, "High": 10.2, "Low": 9.9, "Close": 10.1, "Volume": 1000.0}, index=self.index)
        self.now_ms = int(self.index[-1].timestamp() * 1000)

    def test_closed_bar_window_aliases_and_staleness(self):
        microsense.write_minutes(self.path, "US.TEM", self.frame)
        raw = microsense.read_minutes(self.path, "TEM", self.now_ms)
        self.assertEqual(len(raw), 25)
        self.assertEqual(raw, microsense.read_minutes(self.path, "US.TEM", self.now_ms))
        self.assertEqual(raw[-1][0], self.now_ms - 60_000, "forming bar must not enter the features")
        self.assertEqual(len(microsense.read_minutes(self.path, "TEM", self.now_ms - 60_000)), 24)
        self.assertEqual(microsense.read_minutes(self.path, "TEM", self.now_ms + 360_000), [])
        with sqlite3.connect(self.path) as db:
            db.execute("DELETE FROM bars WHERE ts=?", (self.now_ms - 600_000,))
        self.assertEqual(len(microsense.read_minutes(self.path, "TEM", self.now_ms)), 24)

    def test_minute_store_cleaning_preserves_minutes_and_rejects_bad_bars(self):
        bad = self.frame.copy()
        bad.iloc[0, bad.columns.get_loc("High")] = math.nan
        self.assertEqual(microsense.write_minutes(self.path, "TEM", bad), 25)
        store = CandleStore(self.path, bar_ms=60_000)
        self.addCleanup(store.close)
        self.assertEqual(store.clean()["off_grid"], 0)
        self.assertEqual(len(store.frame("TEM")), 25)

    def test_forward_pass_is_candidate_specific_and_defense_is_orthogonal(self):
        brain = Path(self.tmp.name) / "brain.sqlite3"
        with sqlite3.connect(brain) as db:
            db.execute("CREATE TABLE brain_synapses(source_id TEXT, target_id TEXT, weight REAL)")
            db.executemany("INSERT INTO brain_synapses VALUES(?,?,?)", [
                ("node:sensor:morning_thrust", "node:decision:rank1_execute", 2.0),
                ("node:sensor:spread_drag", "node:reflex:adversarial_bear_sentry", 2.0),
                ("node:decision:rank1_execute", "node:reflex:adversarial_bear_sentry", 1000.0),
            ])
        strong = microsense.forward_pass(brain, {"node:sensor:morning_thrust": 1.0, "node:sensor:spread_drag": 0.1})
        weak = microsense.forward_pass(brain, {"node:sensor:morning_thrust": 0.0, "node:sensor:spread_drag": 0.1})
        self.assertGreater(strong[0], weak[0])
        self.assertEqual(strong[1], weak[1], "bull conviction must never increase bear defense")
        self.assertIsNone(gating.tier3_connectome(*strong))
        self.assertIsNotNone(gating.tier3_connectome(*weak))

    def test_kalman_motion_distinguishes_accelerating_and_falling_prices(self):
        velocity, acceleration = microsense.kalman_motion([i * i / 1000 for i in range(25)])
        self.assertGreater(velocity, 0)
        self.assertGreater(acceleration, 0)
        falling_velocity, falling_acceleration = microsense.kalman_motion([-i * i / 1000 for i in range(25)])
        self.assertLess(falling_velocity, 0)
        self.assertLess(falling_acceleration, 0)

    def test_candidate_capture_uses_native_minutes_and_only_requested_symbols(self):
        from unittest.mock import patch
        feed = Mock()
        feed.ensure.return_value = {"US.TEM"}
        feed.candles.return_value = self.frame
        with patch("papercompmoomoo.microsense.MINUTE_DB", self.path):
            self.assertEqual(microsense.capture_candidate_minutes(feed, ["TEM", "RIVN"]), 26)
        feed.candles.assert_called_once_with("US.TEM")
        feed.release_unused.assert_called_once_with({"US.TEM", "US.RIVN"})

    def test_native_minute_subscription_and_timestamp_do_not_close_a_forming_bar(self):
        from futu import RET_OK, SubType, KLType
        q = Mock()
        q.query_subscription.return_value = (RET_OK, {"remain": 100})
        q.subscribe.return_value = (RET_OK, None)
        feed = MoomooFeed(q, minutes=1)
        self.assertEqual(feed.ensure(["US.TEM"]), {"US.TEM"})
        self.assertEqual(q.subscribe.call_args.args[1], [SubType.K_1M])
        q.get_cur_kline.return_value = (RET_OK, pd.DataFrame({"time_key": ["2026-11-02 10:25:00"], "open": [10], "high": [11], "low": [9], "close": [10], "volume": [100]}))
        frame = feed.candles("US.TEM")
        self.assertEqual(q.get_cur_kline.call_args.kwargs["ktype"], KLType.K_1M)
        self.assertEqual(frame.index[0].minute, 25)

    def test_gainer_defaults_have_no_fifteen_minute_dependency(self):
        settings = Settings()
        self.assertEqual(settings.candle_interval, "1m")
        self.assertEqual(settings.history_db, "data/common1m.sqlite3")
        bot = MomentumPaperBot.__new__(MomentumPaperBot)
        bot.COMMON_DB = self.path
        bot.store = Mock()
        evidence = bot.sense_gainer("US.TEM", self.index[-1], 2.0, {"bid": 10, "ask": 10.01})
        self.assertIn("one-minute bars or live quote", evidence["reason"])
        bot.store.request_hot.assert_called_once()

    def test_real_security_metadata_excludes_non_common_instruments(self):
        listing = pd.DataFrame({"Symbol": ["AAPL", "ETF", "ADR", "WARR", "PREF"], "Test Issue": ["N"] * 5,
                                "ETF": ["N", "Y", "N", "N", "N"], "Security Name": ["Apple - Common Stock", "ETF fund", "Issuer - American Depositary Shares", "Issuer - Warrants", "Issuer - Preferred Stock"]})
        self.assertEqual(nasdaq_listed_symbols(listing), frozenset({"US.AAPL"}))

    def test_non_finite_data_cannot_pass_gates_or_produce_sizing(self):
        for invalid in (math.nan, math.inf, -math.inf):
            self.assertIsNotNone(gating.tier2_inflection(10, 5, invalid, False))
            self.assertIsNotNone(gating.tier3_connectome(invalid, 0))
            self.assertEqual(gating.tier4_notional(invalid, 10000, 5), 0)
        self.assertIsNone(gating.tier2_inflection(10, 25, 1.8, False))
        self.assertLessEqual(gating.slip_limit(3.01), 3.01 * 1.005)
        self.assertGreaterEqual(gating.slip_limit(3.01, side_buy=False), 3.01 * 0.995)


if __name__ == "__main__":
    unittest.main()

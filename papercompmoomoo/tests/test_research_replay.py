"""Exit-grid replay fixtures. No Moomoo connection, orders, or runtime writes."""
import sys
import unittest
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import research_replay as replay


class ExitGridReplay(unittest.TestCase):
    def test_synthetic_gex_uses_only_prior_bars(self):
        engine = replay.CausalSyntheticGEX(strike_step=5.0, half_life_bars=45, warmup_bars=1)
        self.assertEqual(engine.state(0, 100.0)["regime"], "WARMUP")
        engine.observe(0, 100.0, 100.0, 100.0, 100.0)
        before_current_bar = engine.state(1, 100.0)
        self.assertEqual(before_current_bar["regime"], "POSITIVE_GAMMA")
        self.assertAlmostEqual(before_current_bar["gamma_flip"], 100.0)
        self.assertEqual(before_current_bar["call_wall"], 105.0)

    def test_gex_regime_flip_is_a_defensive_exit(self):
        ny = ZoneInfo("America/New_York")
        opened = int(datetime(2026, 11, 6, 10, 21, tzinfo=ny).timestamp() * 1000)
        trade = {"entry": 100.0, "initial_stop": 95.0, "initial_target": 110.0,
                 "trail_pct": 5.0, "opened_ms": opened, "session": "2026-11-06", "symbol": "XYZ"}
        bars = [(opened - (20 - i) * 60_000, 100.0, 101.0, 99.0, 100.0, 1_000.0) for i in range(20)]
        bars += [(opened, 100.0, 101.0, 99.0, 100.0, 1_000.0),
                 (opened + 60_000, 95.0, 96.0, 94.0, 94.0, 1_000.0)]
        result = replay.replay_position(trade, bars, 10.0, grid_config={"gex_stop_buffer": 0.20})
        self.assertEqual(result["replay_reason"], "gex_regime_flip")
        self.assertEqual(result["replay_exit"], 94.0)

    def test_candidate_replay_enters_on_next_open_and_exits_causally(self):
        ny = ZoneInfo("America/New_York")
        first = int(datetime(2026, 11, 6, 10, 0, tzinfo=ny).timestamp() * 1000)
        bars = [(first + i * 60_000, 100.0, 101.0, 99.0, 100.0, 1_000.0) for i in range(20)]
        bars += [(first + 20 * 60_000, 100.0, 108.0, 100.0, 107.0, 1_000.0),
                 (first + 21 * 60_000, 106.0, 107.0, 99.0, 99.0, 1_000.0)]
        result = replay.replay_candidate_entry_exit("XYZ", bars, first)
        self.assertEqual(result["status"], "entry_exit_replayed")
        self.assertEqual(result["entry_price"], 106.0)
        self.assertEqual(result["exit_reason"], "gex_regime_flip")

    def test_randomized_candidates_are_10000_unique_repeatable_and_include_the_live_baseline(self):
        as_of = 1_794_000_000_000
        a = replay.random_grid_candidates("2026-11-06", as_of)
        b = replay.random_grid_candidates("2026-11-06", as_of)
        self.assertEqual(len(a), 10_000)
        self.assertEqual(a, b)
        keys = {(g["stop_scale"], g["trail_scale"], g["target_scale"], g["target_70_fraction"], tuple(g["profit_line_fractions"])) for g in a}
        self.assertEqual(len(keys), 10_000)
        self.assertEqual(a[0], {"stop_scale": 1.0, "trail_scale": 1.0, "target_scale": 1.0,
                                "target_70_fraction": 0.70, "profit_line_fractions": [0.5]})

    def test_profit_lines_are_ordered_inside_midpoint_and_70_percent(self):
        lines = replay._profit_ladder(70.0, 50.0, [0.1, 0.9, 0.5])
        self.assertEqual(lines, [68.0, 60.0, 52.0])
        self.assertTrue(all(50.0 < line < 70.0 for line in lines))

    def test_grid_ratchets_up_and_exits_down_at_the_70_percent_line(self):
        ny = ZoneInfo("America/New_York")
        trade = {"entry": 100.0, "initial_stop": 95.0, "initial_target": 110.0,
                 "trail_pct": 5.0, "opened_ms": int(datetime(2026, 11, 6, 10, 1, tzinfo=ny).timestamp() * 1000),
                 "session": "2026-11-06", "symbol": "XYZ"}
        bars = [
            (trade["opened_ms"] + 60_000, 106.0, 108.0, 106.0, 107.0, 1_000.0),
            (trade["opened_ms"] + 120_000, 107.0, 109.0, 105.5, 106.0, 1_200.0),
        ]
        result = replay.replay_position(trade, bars, 10.0, grid_config={
            "stop_scale": 1.0, "trail_scale": 1.0, "target_scale": 1.0,
            "target_70_fraction": 0.70, "profit_line_fractions": [0.5],
        })
        self.assertEqual(result["replay_reason"], "profit_target_70")
        self.assertAlmostEqual(result["protective_stop"], 102.6)
        self.assertAlmostEqual(result["target_100"], 110.0)
        self.assertAlmostEqual(result["target_70"], 107.0)
        self.assertAlmostEqual(result["midpoint"], 104.8)
        self.assertAlmostEqual(result["additional_profit_line_levels"][0], 105.9)

    def test_100_percent_target_exits_on_an_upward_touch(self):
        ny = ZoneInfo("America/New_York")
        opened = int(datetime(2026, 11, 6, 10, 1, tzinfo=ny).timestamp() * 1000)
        trade = {"entry": 100.0, "initial_stop": 95.0, "initial_target": 110.0,
                 "trail_pct": 5.0, "opened_ms": opened, "session": "2026-11-06", "symbol": "XYZ"}
        result = replay.replay_position(trade, [(opened + 60_000, 108.0, 110.0, 107.0, 109.0, 900.0)], 10.0)
        self.assertEqual(result["replay_reason"], "profit_target_100")
        self.assertAlmostEqual(result["replay_exit"], 110.0)


if __name__ == "__main__":
    unittest.main()

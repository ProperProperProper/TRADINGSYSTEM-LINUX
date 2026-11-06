"""Exit-grid replay fixtures. No Moomoo connection, orders, or runtime writes."""
import sys
import unittest
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import research_replay as replay


class ExitGridReplay(unittest.TestCase):
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

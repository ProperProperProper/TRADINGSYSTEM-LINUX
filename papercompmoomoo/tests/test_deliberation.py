import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import deliberation


class TestDeliberation(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.dir = Path(self.temp_dir.name)
        self.journal_path = self.dir / "journal.sqlite3"
        self.common_path = self.dir / "common1m.sqlite3"
        self.policy_path = self.dir / "live_policy.json"
        self.brain_path = self.dir / "brain.sqlite3"

        # Create mock journal database
        with sqlite3.connect(self.journal_path) as conn:
            conn.execute("""
                CREATE TABLE trades(
                    id INTEGER PRIMARY KEY, symbol TEXT, qty INTEGER, entry REAL, exit_price REAL,
                    pnl_pct REAL, reason TEXT, closed_ms INTEGER
                )
            """)
            conn.execute("""
                CREATE TABLE scout_learnings(
                    id INTEGER PRIMARY KEY, ts_ms INTEGER, symbol TEXT, signal_id INTEGER,
                    confidence REAL, terms_json TEXT, rvol REAL, move_pct REAL, spread_pct REAL,
                    pnl_pct REAL, minutes_held REAL, exit_reason TEXT, lesson TEXT, source TEXT
                )
            """)
            conn.execute("""
                INSERT INTO trades(symbol, qty, entry, exit_price, pnl_pct, reason, closed_ms)
                VALUES('US.TEST', 100, 10.0, 11.0, 10.0, 'take profit', 1793000000000),
                      ('US.LOSS', 100, 20.0, 19.5, -2.5, 'stop', 1793000000000)
            """)
            conn.execute("""
                INSERT INTO scout_learnings(lesson) VALUES('prior test lesson')
            """)

        # Create mock brain database
        with sqlite3.connect(self.brain_path) as conn:
            conn.execute("""
                CREATE TABLE memories(
                    id INTEGER PRIMARY KEY, ts_ms INTEGER, agent TEXT, kind TEXT, topic TEXT,
                    text TEXT, evidence TEXT, confidence REAL, source TEXT, via TEXT
                )
            """)

    def tearDown(self):
        self.temp_dir.cleanup()

    def test_gather_evidence(self):
        ev = deliberation.gather_evidence(journal_path=self.journal_path)
        self.assertEqual(ev["closed_trades_count"], 2)
        self.assertEqual(ev["win_rate"], 0.5)
        self.assertGreater(ev["total_dollar_pnl"], 0.0)
        self.assertEqual(len(ev["recent_trades"]), 2)
        self.assertEqual(len(ev["recent_lessons"]), 1)

    def test_parse_model_response(self):
        sample_json = json.dumps({
            "min_rvol": 2.5,
            "min_clv": 0.75,
            "max_spread_pct": 0.003,
            "allocation_mode": "dynamic_conviction_portfolio",
            "rank_weights": [0.60, 0.25, 0.15],
            "pyramid_enabled": True,
            "pyramid_trigger_pct": 0.010,
            "trailing_stop_pct": 0.025,
            "arm_trailing_pct": 0.015,
            "rationale": "High conviction breakout sizing"
        })
        stance = deliberation.parse_model_response("claude", f"Here is my recommendation:\n{sample_json}\nDone.")
        self.assertEqual(stance.backend, "claude")
        self.assertEqual(stance.min_rvol, 2.5)
        self.assertEqual(stance.min_clv, 0.75)
        self.assertEqual(stance.rank_weights, [0.60, 0.25, 0.15])
        self.assertTrue(stance.pyramid_enabled)

    def test_synthesize_gemini_verdict_fallback(self):
        stances = {
            "claude": deliberation.ModelStance("claude", 2.0, 0.70, 0.0035, "mode", [0.5, 0.3, 0.2], True, 0.01, 0.025, 0.015, "rat"),
            "codex": deliberation.ModelStance("codex", 2.4, 0.72, 0.0035, "mode", [0.5, 0.3, 0.2], True, 0.01, 0.025, 0.015, "rat"),
        }
        verdict = deliberation.synthesize_gemini_verdict(
            stances, {}, call=lambda _prompt: (_ for _ in ()).throw(RuntimeError("offline test"))
        )
        self.assertIn("min_rvol", verdict)
        self.assertAlmostEqual(verdict["min_rvol"], 2.2, places=1)
        self.assertAlmostEqual(verdict["min_clv"], 0.71, places=1)
        self.assertEqual(len(verdict["rank_weights"]), 3)

    def test_apply_verdict_and_learn_success(self):
        verdict = {
            "strategy_mode": "dynamic_conviction_portfolio",
            "min_rvol": 2.2,
            "min_clv": 0.70,
            "max_spread_pct": 0.0035,
            "rank_weights": [0.50, 0.30, 0.20],
            "pyramid_enabled": True,
            "pyramid_trigger_pct": 0.010,
            "pyramid_add_pct": 0.50,
            "trailing_stop_pct": 0.025,
            "arm_trailing_pct": 0.015,
            "rationale": "Test policy"
        }
        backtest = {
            "passed": True,
            "trades_count": 25,
            "win_rate": 0.65,
            "profit_factor": 1.75,
            "total_pnl_pct": 12.5,
            "baseline_pnl_pct": 4.0,
            "alpha_pct": 8.5,
            "sharpe_ratio": 1.9,
        }
        applied = deliberation.apply_verdict_and_learn(
            verdict, backtest, policy_path=self.policy_path,
            journal_path=self.journal_path, brain_path=self.brain_path
        )
        self.assertTrue(applied)
        self.assertTrue(self.policy_path.exists())
        loaded = deliberation.load_active_policy(policy_path=self.policy_path)
        self.assertEqual(loaded["min_rvol"], 2.2)
        self.assertEqual(loaded["rank_weights"], [0.50, 0.30, 0.20])

        # Verify entry in scout_learnings
        with sqlite3.connect(self.journal_path) as conn:
            row = conn.execute("SELECT lesson FROM scout_learnings WHERE source='gemini_consensus'").fetchone()
            self.assertIsNotNone(row)
            self.assertIn("APPROVED & APPLIED", row[0])


if __name__ == "__main__":
    unittest.main()
